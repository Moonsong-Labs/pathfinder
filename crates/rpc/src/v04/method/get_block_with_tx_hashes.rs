use anyhow::Context;
use pathfinder_common::BlockId;
use serde::Deserialize;

use crate::context::RpcContext;
use crate::v02::types::reply::BlockStatus;

#[derive(Deserialize, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Copy, Clone))]
#[serde(deny_unknown_fields)]
pub struct GetBlockInput {
    block_id: BlockId,
}

crate::error::generate_rpc_error_subset!(GetBlockError: BlockNotFound);

/// Get block information with transaction hashes given the block id
pub async fn get_block_with_tx_hashes(
    context: RpcContext,
    input: GetBlockInput,
) -> Result<types::Block, GetBlockError> {
    let storage = context.storage.clone();
    let span = tracing::Span::current();

    tokio::task::spawn_blocking(move || {
        let _g = span.enter();
        let mut connection = storage
            .connection()
            .context("Opening database connection")?;

        let transaction = connection
            .transaction()
            .context("Creating database transaction")?;

        let block_id = match input.block_id {
            BlockId::Pending => {
                let block = context
                    .pending_data
                    .get(&transaction)
                    .context("Querying pending data")?;

                let header = block.header();

                let transactions = block.block.transactions.iter().map(|t| t.hash).collect();

                return Ok(types::Block::from_parts(
                    header,
                    BlockStatus::Pending,
                    transactions,
                ));
            }
            other => other.try_into().expect("Only pending cast should fail"),
        };

        let header = transaction
            .block_header(block_id)
            .context("Reading block from database")?
            .ok_or(GetBlockError::BlockNotFound)?;

        let l1_accepted = transaction.block_is_l1_accepted(header.number.into())?;
        let block_status = if l1_accepted {
            BlockStatus::AcceptedOnL1
        } else {
            BlockStatus::AcceptedOnL2
        };

        let transactions = transaction
            .transaction_hashes_for_block(header.number.into())
            .context("Reading transaction hashes")?
            .context("Missing block")?;

        Ok(types::Block::from_parts(header, block_status, transactions))
    })
    .await
    .context("Database read panic or shutting down")?
}

mod types {
    use pathfinder_common::{
        BlockHash, BlockHeader, BlockNumber, BlockTimestamp, SequencerAddress, StateCommitment,
        TransactionHash,
    };
    use serde::Serialize;
    use serde_with::{serde_as, skip_serializing_none};

    use crate::felt::RpcFelt;
    use crate::v02::types::reply::BlockStatus;

    /// L2 Block as returned by the RPC API.
    #[serde_as]
    #[skip_serializing_none]
    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct Block {
        pub status: BlockStatus,
        #[serde_as(as = "Option<RpcFelt>")]
        pub block_hash: Option<BlockHash>,
        #[serde_as(as = "RpcFelt")]
        pub parent_hash: BlockHash,
        pub block_number: Option<BlockNumber>,
        #[serde_as(as = "Option<RpcFelt>")]
        pub new_root: Option<StateCommitment>,
        pub timestamp: BlockTimestamp,
        #[serde_as(as = "RpcFelt")]
        pub sequencer_address: SequencerAddress,

        pub transactions: Vec<TransactionHash>,
    }

    impl Block {
        pub fn from_parts(
            header: BlockHeader,
            status: BlockStatus,
            transactions: Vec<TransactionHash>,
        ) -> Self {
            Self {
                status,
                block_hash: Some(header.hash),
                parent_hash: header.parent_hash,
                block_number: Some(header.number),
                new_root: Some(header.state_commitment),
                timestamp: header.timestamp,
                sequencer_address: header.sequencer_address,
                transactions,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pathfinder_common::macro_prelude::*;
    use pathfinder_common::BlockNumber;
    use serde_json::json;

    use super::*;

    #[rstest::rstest]
    #[case::pending_by_position(json!(["pending"]), BlockId::Pending)]
    #[case::pending_by_name(json!({"block_id": "pending"}), BlockId::Pending)]
    #[case::latest_by_position(json!(["latest"]), BlockId::Latest)]
    #[case::latest_by_name(json!({"block_id": "latest"}), BlockId::Latest)]
    #[case::number_by_position(json!([{"block_number":123}]), BlockNumber::new_or_panic(123).into())]
    #[case::number_by_name(json!({"block_id": {"block_number":123}}), BlockNumber::new_or_panic(123).into())]
    #[case::hash_by_position(json!([{"block_hash": "0xbeef"}]), block_hash!("0xbeef").into())]
    #[case::hash_by_name(json!({"block_id": {"block_hash": "0xbeef"}}), block_hash!("0xbeef").into())]
    fn input_parsing(#[case] input: serde_json::Value, #[case] block_id: BlockId) {
        let input = serde_json::from_value::<GetBlockInput>(input).unwrap();

        let expected = GetBlockInput { block_id };

        assert_eq!(input, expected);
    }

    #[tokio::test]
    async fn pending() {
        let context = RpcContext::for_tests_with_pending().await;

        let result = get_block_with_tx_hashes(
            context,
            GetBlockInput {
                block_id: BlockId::Pending,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.parent_hash, block_hash_bytes!(b"latest"));
    }

    #[tokio::test]
    async fn latest() {
        let context = RpcContext::for_tests_with_pending().await;

        let result = get_block_with_tx_hashes(
            context,
            GetBlockInput {
                block_id: BlockId::Latest,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.block_hash, Some(block_hash_bytes!(b"latest")));
    }

    #[tokio::test]
    async fn by_number() {
        let context = RpcContext::for_tests_with_pending().await;

        let result = get_block_with_tx_hashes(
            context,
            GetBlockInput {
                block_id: BlockId::Number(BlockNumber::GENESIS),
            },
        )
        .await
        .unwrap();

        assert_eq!(result.block_hash, Some(block_hash_bytes!(b"genesis")));
    }

    #[tokio::test]
    async fn by_hash() {
        let context = RpcContext::for_tests_with_pending().await;

        let result = get_block_with_tx_hashes(
            context,
            GetBlockInput {
                block_id: BlockId::Hash(block_hash_bytes!(b"genesis")),
            },
        )
        .await
        .unwrap();

        assert_eq!(result.block_hash, Some(block_hash_bytes!(b"genesis")));
    }

    #[tokio::test]
    async fn not_found_by_number() {
        let context = RpcContext::for_tests_with_pending().await;

        let result = get_block_with_tx_hashes(
            context,
            GetBlockInput {
                block_id: BlockId::Number(BlockNumber::MAX),
            },
        )
        .await;

        assert_matches::assert_matches!(result, Err(GetBlockError::BlockNotFound));
    }

    #[tokio::test]
    async fn not_found_by_hash() {
        let context = RpcContext::for_tests_with_pending().await;

        let result = get_block_with_tx_hashes(
            context,
            GetBlockInput {
                block_id: BlockId::Hash(block_hash_bytes!(b"non-existent")),
            },
        )
        .await;

        assert_matches::assert_matches!(result, Err(GetBlockError::BlockNotFound));
    }

    #[tokio::test]
    async fn transaction_hashes_are_correct() {
        let ctx: RpcContext = RpcContext::for_tests();

        let tx_hashes = get_block_with_tx_hashes(
            ctx,
            GetBlockInput {
                block_id: BlockNumber::new_or_panic(1).into(),
            },
        )
        .await
        .unwrap()
        .transactions;

        assert_eq!(
            tx_hashes,
            vec![
                transaction_hash_bytes!(b"txn 1"),
                transaction_hash_bytes!(b"txn 2")
            ]
        );
    }
}
