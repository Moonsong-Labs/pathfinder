use anyhow::Context;
use pathfinder_common::TransactionHash;

use crate::context::RpcContext;

#[derive(serde::Deserialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GetTransactionReceiptInput {
    transaction_hash: TransactionHash,
}

crate::error::generate_rpc_error_subset!(GetTransactionReceiptError: TxnHashNotFound);

pub async fn get_transaction_receipt(
    context: RpcContext,
    input: GetTransactionReceiptInput,
) -> Result<types::MaybePendingTransactionReceipt, GetTransactionReceiptError> {
    let storage = context.storage.clone();
    let span = tracing::Span::current();

    let jh = tokio::task::spawn_blocking(move || {
        let _g = span.enter();
        let mut db = storage
            .connection()
            .context("Opening database connection")?;

        let db_tx = db.transaction().context("Creating database transaction")?;

        // Check pending transactions.
        let pending = context
            .pending_data
            .get(&db_tx)
            .context("Querying pending data")?;

        if let Some((transaction, (receipt, events))) = pending
            .block
            .transactions
            .iter()
            .zip(pending.block.transaction_receipts.iter())
            .find_map(|(t, r)| (t.hash == input.transaction_hash).then(|| (t.clone(), r.clone())))
        {
            let pending = types::PendingTransactionReceipt::from(receipt, events, &transaction);
            return Ok(types::MaybePendingTransactionReceipt::Pending(pending));
        }

        let (transaction, receipt, events, block_number) = db_tx
            .transaction_with_receipt(input.transaction_hash)
            .context("Reading transaction receipt from database")?
            .ok_or(GetTransactionReceiptError::TxnHashNotFound)?;

        let block_hash = db_tx
            .block_hash(block_number.into())
            .context("Querying block hash")?
            .context("Block hash info missing")?;

        let l1_accepted = db_tx
            .block_is_l1_accepted(block_number.into())
            .context("Querying block status")?;

        let finality_status = if l1_accepted {
            types::FinalityStatus::AcceptedOnL1
        } else {
            types::FinalityStatus::AcceptedOnL2
        };

        Ok(types::MaybePendingTransactionReceipt::Normal(
            types::TransactionReceipt::with_block_data(
                receipt,
                events,
                finality_status,
                block_hash,
                block_number,
                transaction,
            ),
        ))
    });

    jh.await.context("Database read panic or shutting down")?
}

pub mod types {
    use pathfinder_common::{
        BlockHash, BlockNumber, ContractAddress, EventData, EventKey, Fee,
        L2ToL1MessagePayloadElem, TransactionHash,
    };
    use serde::Serialize;
    use serde_with::serde_as;

    use crate::felt::{RpcFelt, RpcFelt251};
    use crate::v02::types::reply::BlockStatus;

    /// L2 transaction receipt as returned by the RPC API.
    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    #[serde(untagged)]
    pub enum MaybePendingTransactionReceipt {
        Normal(TransactionReceipt),
        Pending(PendingTransactionReceipt),
    }

    /// Non-pending L2 transaction receipt as returned by the RPC API.
    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    #[serde(tag = "type")]
    pub enum TransactionReceipt {
        #[serde(rename = "INVOKE")]
        Invoke(InvokeTransactionReceipt),
        #[serde(rename = "DECLARE")]
        Declare(DeclareTransactionReceipt),
        #[serde(rename = "L1_HANDLER")]
        L1Handler(L1HandlerTransactionReceipt),
        // FIXME regenesis: remove Deploy receipt type after regenesis
        // We are keeping this type of receipt until regenesis
        // only to support older pre-0.11.0 blocks
        #[serde(rename = "DEPLOY")]
        Deploy(DeployTransactionReceipt),
        #[serde(rename = "DEPLOY_ACCOUNT")]
        DeployAccount(DeployAccountTransactionReceipt),
    }

    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct InvokeTransactionReceipt {
        #[serde(flatten)]
        pub common: CommonTransactionReceiptProperties,
    }

    #[serde_as]
    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct CommonTransactionReceiptProperties {
        #[serde_as(as = "RpcFelt")]
        pub transaction_hash: TransactionHash,
        pub actual_fee: Fee,
        #[serde_as(as = "RpcFelt")]
        pub block_hash: BlockHash,
        pub block_number: BlockNumber,
        pub messages_sent: Vec<MessageToL1>,
        pub events: Vec<Event>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub revert_reason: Option<String>,
        pub execution_status: ExecutionStatus,
        pub finality_status: FinalityStatus,
    }

    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub enum ExecutionStatus {
        Succeeded,
        Reverted,
    }

    impl From<pathfinder_common::receipt::ExecutionStatus> for ExecutionStatus {
        fn from(value: pathfinder_common::receipt::ExecutionStatus) -> Self {
            match value {
                pathfinder_common::receipt::ExecutionStatus::Succeeded => Self::Succeeded,
                pathfinder_common::receipt::ExecutionStatus::Reverted { .. } => Self::Reverted,
            }
        }
    }

    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub enum FinalityStatus {
        AcceptedOnL2,
        AcceptedOnL1,
    }

    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct L1HandlerTransactionReceipt {
        #[serde(flatten)]
        pub common: CommonTransactionReceiptProperties,
    }

    #[serde_as]
    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct DeployTransactionReceipt {
        #[serde(flatten)]
        pub common: CommonTransactionReceiptProperties,
        #[serde_as(as = "RpcFelt251")]
        pub contract_address: ContractAddress,
    }

    #[serde_as]
    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct DeployAccountTransactionReceipt {
        #[serde(flatten)]
        pub common: CommonTransactionReceiptProperties,
        #[serde_as(as = "RpcFelt251")]
        pub contract_address: ContractAddress,
    }

    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct DeclareTransactionReceipt {
        #[serde(flatten)]
        pub common: CommonTransactionReceiptProperties,
    }

    impl TransactionReceipt {
        pub fn with_block_data(
            receipt: pathfinder_common::receipt::Receipt,
            events: Vec<pathfinder_common::event::Event>,
            finality_status: FinalityStatus,
            block_hash: BlockHash,
            block_number: BlockNumber,
            transaction: pathfinder_common::transaction::Transaction,
        ) -> Self {
            let revert_reason = receipt.revert_reason().map(ToOwned::to_owned);
            let common = CommonTransactionReceiptProperties {
                transaction_hash: receipt.transaction_hash,
                actual_fee: receipt.actual_fee,
                block_hash,
                block_number,
                messages_sent: receipt
                    .l2_to_l1_messages
                    .into_iter()
                    .map(MessageToL1::from)
                    .collect(),
                events: events.into_iter().map(Event::from).collect(),
                revert_reason,
                execution_status: receipt.execution_status.into(),
                finality_status,
            };

            use pathfinder_common::transaction::TransactionVariant;
            match transaction.variant {
                TransactionVariant::DeclareV0(_)
                | TransactionVariant::DeclareV1(_)
                | TransactionVariant::DeclareV2(_)
                | TransactionVariant::DeclareV3(_) => {
                    Self::Declare(DeclareTransactionReceipt { common })
                }
                TransactionVariant::DeployV0(tx) => Self::Deploy(DeployTransactionReceipt {
                    common,
                    contract_address: tx.contract_address,
                }),
                TransactionVariant::DeployV1(tx) => Self::Deploy(DeployTransactionReceipt {
                    common,
                    contract_address: tx.contract_address,
                }),
                TransactionVariant::DeployAccountV1(tx) => {
                    Self::DeployAccount(DeployAccountTransactionReceipt {
                        common,
                        contract_address: tx.contract_address,
                    })
                }
                TransactionVariant::DeployAccountV3(tx) => {
                    Self::DeployAccount(DeployAccountTransactionReceipt {
                        common,
                        contract_address: tx.contract_address,
                    })
                }
                TransactionVariant::InvokeV0(_)
                | TransactionVariant::InvokeV1(_)
                | TransactionVariant::InvokeV3(_) => {
                    Self::Invoke(InvokeTransactionReceipt { common })
                }
                TransactionVariant::L1Handler(_) => {
                    Self::L1Handler(L1HandlerTransactionReceipt { common })
                }
            }
        }
    }

    /// Non-pending L2 transaction receipt as returned by the RPC API.
    ///
    /// Pending receipts don't have status, status_data, block_hash,
    /// block_number fields
    #[serde_as]
    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    #[serde(tag = "type")]
    pub enum PendingTransactionReceipt {
        #[serde(rename = "INVOKE")]
        Invoke(PendingInvokeTransactionReceipt),
        #[serde(rename = "DECLARE")]
        Declare(PendingDeclareTransactionReceipt),
        #[serde(rename = "DEPLOY")]
        Deploy(PendingDeployTransactionReceipt),
        #[serde(rename = "DEPLOY_ACCOUNT")]
        DeployAccount(PendingDeployAccountTransactionReceipt),
        #[serde(rename = "L1_HANDLER")]
        L1Handler(PendingL1HandlerTransactionReceipt),
    }

    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct PendingInvokeTransactionReceipt {
        #[serde(flatten)]
        pub common: CommonPendingTransactionReceiptProperties,
    }
    #[serde_as]
    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct CommonPendingTransactionReceiptProperties {
        pub transaction_hash: TransactionHash,
        pub actual_fee: Fee,
        pub messages_sent: Vec<MessageToL1>,
        pub events: Vec<Event>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub revert_reason: Option<String>,
        pub execution_status: ExecutionStatus,
        pub finality_status: FinalityStatus,
    }

    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct PendingDeclareTransactionReceipt {
        #[serde(flatten)]
        pub common: CommonPendingTransactionReceiptProperties,
    }

    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct PendingDeployTransactionReceipt {
        #[serde(flatten)]
        pub common: CommonPendingTransactionReceiptProperties,

        pub contract_address: ContractAddress,
    }

    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct PendingDeployAccountTransactionReceipt {
        #[serde(flatten)]
        pub common: CommonPendingTransactionReceiptProperties,

        pub contract_address: ContractAddress,
    }

    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    pub struct PendingL1HandlerTransactionReceipt {
        #[serde(flatten)]
        pub common: CommonPendingTransactionReceiptProperties,
    }

    impl PendingTransactionReceipt {
        pub fn from(
            receipt: pathfinder_common::receipt::Receipt,
            events: Vec<pathfinder_common::event::Event>,
            transaction: &pathfinder_common::transaction::Transaction,
        ) -> Self {
            let revert_reason = receipt.revert_reason().map(ToOwned::to_owned);
            let common = CommonPendingTransactionReceiptProperties {
                transaction_hash: receipt.transaction_hash,
                actual_fee: receipt.actual_fee,
                messages_sent: receipt
                    .l2_to_l1_messages
                    .into_iter()
                    .map(MessageToL1::from)
                    .collect(),
                events: events.into_iter().map(Event::from).collect(),
                revert_reason,
                execution_status: receipt.execution_status.into(),
                finality_status: FinalityStatus::AcceptedOnL2,
            };

            use pathfinder_common::transaction::TransactionVariant;
            match &transaction.variant {
                TransactionVariant::DeclareV0(_)
                | TransactionVariant::DeclareV1(_)
                | TransactionVariant::DeclareV2(_)
                | TransactionVariant::DeclareV3(_) => {
                    Self::Declare(PendingDeclareTransactionReceipt { common })
                }
                TransactionVariant::DeployV0(tx) => Self::Deploy(PendingDeployTransactionReceipt {
                    common,
                    contract_address: tx.contract_address,
                }),
                TransactionVariant::DeployV1(tx) => Self::Deploy(PendingDeployTransactionReceipt {
                    common,
                    contract_address: tx.contract_address,
                }),
                TransactionVariant::DeployAccountV1(tx) => {
                    Self::DeployAccount(PendingDeployAccountTransactionReceipt {
                        common,
                        contract_address: tx.contract_address,
                    })
                }
                TransactionVariant::DeployAccountV3(tx) => {
                    Self::DeployAccount(PendingDeployAccountTransactionReceipt {
                        common,
                        contract_address: tx.contract_address,
                    })
                }
                TransactionVariant::InvokeV0(_)
                | TransactionVariant::InvokeV1(_)
                | TransactionVariant::InvokeV3(_) => {
                    Self::Invoke(PendingInvokeTransactionReceipt { common })
                }
                TransactionVariant::L1Handler(_) => {
                    Self::L1Handler(PendingL1HandlerTransactionReceipt { common })
                }
            }
        }
    }

    /// Message sent from L2 to L1.
    #[serde_as]
    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    #[serde(deny_unknown_fields)]
    pub struct MessageToL1 {
        pub from_address: ContractAddress,
        pub to_address: ContractAddress,
        #[serde_as(as = "Vec<RpcFelt>")]
        pub payload: Vec<L2ToL1MessagePayloadElem>,
    }

    impl MessageToL1 {
        fn from(msg: pathfinder_common::receipt::L2ToL1Message) -> Self {
            Self {
                from_address: msg.from_address,
                to_address: msg.to_address,
                payload: msg.payload,
            }
        }
    }

    /// Event emitted as a part of a transaction.
    #[serde_as]
    #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    #[serde(deny_unknown_fields)]
    pub struct Event {
        #[serde_as(as = "RpcFelt251")]
        pub from_address: ContractAddress,
        #[serde_as(as = "Vec<RpcFelt>")]
        pub keys: Vec<EventKey>,
        #[serde_as(as = "Vec<RpcFelt>")]
        pub data: Vec<EventData>,
    }

    impl From<pathfinder_common::event::Event> for Event {
        fn from(e: pathfinder_common::event::Event) -> Self {
            Self {
                from_address: e.from_address,
                keys: e.keys,
                data: e.data,
            }
        }
    }

    /// Represents transaction status.
    #[derive(Copy, Clone, Debug, Serialize, PartialEq, Eq)]
    #[cfg_attr(any(test, feature = "rpc-full-serde"), derive(serde::Deserialize))]
    #[serde(deny_unknown_fields)]
    pub enum TransactionStatus {
        #[serde(rename = "ACCEPTED_ON_L2")]
        AcceptedOnL2,
        #[serde(rename = "ACCEPTED_ON_L1")]
        AcceptedOnL1,
        #[serde(rename = "REJECTED")]
        Rejected,
    }

    impl From<BlockStatus> for TransactionStatus {
        fn from(status: BlockStatus) -> Self {
            match status {
                BlockStatus::Pending => TransactionStatus::AcceptedOnL2,
                BlockStatus::AcceptedOnL2 => TransactionStatus::AcceptedOnL2,
                BlockStatus::AcceptedOnL1 => TransactionStatus::AcceptedOnL1,
                BlockStatus::Rejected => TransactionStatus::Rejected,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // TODO: add serialization tests for each receipt variant..

    use pathfinder_common::macro_prelude::*;
    use pathfinder_common::{BlockNumber, Fee};

    use super::*;

    mod parsing {
        use serde_json::json;

        use super::*;

        #[test]
        fn positional_args() {
            let positional = json!(["0xdeadbeef"]);

            let input = serde_json::from_value::<GetTransactionReceiptInput>(positional).unwrap();
            assert_eq!(
                input,
                GetTransactionReceiptInput {
                    transaction_hash: transaction_hash!("0xdeadbeef")
                }
            )
        }

        #[test]
        fn named_args() {
            let named_args = json!({
                "transaction_hash": "0xdeadbeef"
            });

            let input = serde_json::from_value::<GetTransactionReceiptInput>(named_args).unwrap();
            assert_eq!(
                input,
                GetTransactionReceiptInput {
                    transaction_hash: transaction_hash!("0xdeadbeef")
                }
            )
        }
    }

    mod errors {
        use super::*;

        #[tokio::test]
        async fn hash_not_found() {
            let context = RpcContext::for_tests();
            let input = GetTransactionReceiptInput {
                transaction_hash: transaction_hash_bytes!(b"non_existent"),
            };

            let result = get_transaction_receipt(context, input).await;

            assert_matches::assert_matches!(
                result,
                Err(GetTransactionReceiptError::TxnHashNotFound)
            );
        }
    }

    #[tokio::test]
    async fn success() {
        let context = RpcContext::for_tests();
        let input = GetTransactionReceiptInput {
            transaction_hash: transaction_hash_bytes!(b"txn 0"),
        };

        let result = get_transaction_receipt(context, input).await.unwrap();
        use types::*;
        assert_eq!(
            result,
            MaybePendingTransactionReceipt::Normal(TransactionReceipt::Invoke(
                InvokeTransactionReceipt {
                    common: CommonTransactionReceiptProperties {
                        transaction_hash: transaction_hash_bytes!(b"txn 0"),
                        actual_fee: Fee::ZERO,
                        block_hash: block_hash_bytes!(b"genesis"),
                        block_number: BlockNumber::new_or_panic(0),
                        messages_sent: vec![],
                        events: vec![Event {
                            data: vec![event_data_bytes!(b"event 0 data")],
                            from_address: contract_address_bytes!(b"event 0 from addr"),
                            keys: vec![event_key_bytes!(b"event 0 key")],
                        }],
                        execution_status: ExecutionStatus::Succeeded,
                        finality_status: FinalityStatus::AcceptedOnL1,
                        revert_reason: None,
                    }
                }
            ))
        )
    }

    #[tokio::test]
    async fn success_v03() {
        let context = RpcContext::for_tests();
        let input = GetTransactionReceiptInput {
            transaction_hash: transaction_hash_bytes!(b"txn 6"),
        };

        let result = get_transaction_receipt(context, input).await.unwrap();
        use types::*;
        assert_eq!(
            result,
            MaybePendingTransactionReceipt::Normal(TransactionReceipt::Invoke(
                InvokeTransactionReceipt {
                    common: CommonTransactionReceiptProperties {
                        transaction_hash: transaction_hash_bytes!(b"txn 6"),
                        actual_fee: Fee::ZERO,
                        block_hash: block_hash_bytes!(b"latest"),
                        block_number: BlockNumber::new_or_panic(2),
                        messages_sent: vec![MessageToL1 {
                            from_address: contract_address!("0xcafebabe"),
                            to_address: pathfinder_common::ContractAddress::ZERO,
                            payload: vec![
                                l2_to_l1_message_payload_elem!("0x1"),
                                l2_to_l1_message_payload_elem!("0x2"),
                                l2_to_l1_message_payload_elem!("0x3"),
                            ],
                        }],
                        events: vec![],
                        execution_status: ExecutionStatus::Succeeded,
                        finality_status: FinalityStatus::AcceptedOnL2,
                        revert_reason: None,
                    }
                }
            ))
        )
    }

    #[tokio::test]
    async fn pending() {
        let context = RpcContext::for_tests_with_pending().await;
        let transaction_hash = transaction_hash_bytes!(b"pending tx hash 0");
        let input = GetTransactionReceiptInput { transaction_hash };

        let result = get_transaction_receipt(context, input).await.unwrap();
        use types::*;
        assert_eq!(
            result,
            MaybePendingTransactionReceipt::Pending(PendingTransactionReceipt::Invoke(
                PendingInvokeTransactionReceipt {
                    common: CommonPendingTransactionReceiptProperties {
                        transaction_hash,
                        actual_fee: Fee::ZERO,
                        messages_sent: vec![],
                        events: vec![
                            Event {
                                data: vec![],
                                from_address: contract_address!("0xabcddddddd"),
                                keys: vec![event_key_bytes!(b"pending key")],
                            },
                            Event {
                                data: vec![],
                                from_address: contract_address!("0xabcddddddd"),
                                keys: vec![
                                    event_key_bytes!(b"pending key"),
                                    event_key_bytes!(b"second pending key")
                                ],
                            },
                            Event {
                                data: vec![],
                                from_address: contract_address!("0xabcaaaaaaa"),
                                keys: vec![event_key_bytes!(b"pending key 2")],
                            },
                        ],
                        revert_reason: None,
                        execution_status: ExecutionStatus::Succeeded,
                        finality_status: FinalityStatus::AcceptedOnL2
                    }
                }
            ))
        );
    }

    #[tokio::test]
    async fn reverted() {
        let context = RpcContext::for_tests_with_pending().await;
        let input = GetTransactionReceiptInput {
            transaction_hash: transaction_hash_bytes!(b"txn reverted"),
        };
        // Should be a reverted invoke receipt.
        let receipt = get_transaction_receipt(context.clone(), input)
            .await
            .unwrap();

        let receipt = match receipt {
            types::MaybePendingTransactionReceipt::Normal(types::TransactionReceipt::Invoke(x)) => {
                x
            }
            _ => panic!(),
        };

        assert_eq!(
            receipt.common.execution_status,
            types::ExecutionStatus::Reverted
        );

        let input = GetTransactionReceiptInput {
            transaction_hash: transaction_hash_bytes!(b"pending reverted"),
        };

        // Should be a reverted pending invoke receipt.
        let receipt = get_transaction_receipt(context, input).await.unwrap();

        let receipt = match receipt {
            types::MaybePendingTransactionReceipt::Pending(
                types::PendingTransactionReceipt::Invoke(x),
            ) => x,
            _ => panic!(),
        };

        assert_eq!(
            receipt.common.execution_status,
            types::ExecutionStatus::Reverted
        );
    }

    #[tokio::test]
    async fn json_output() {
        let context = RpcContext::for_tests_with_pending().await;
        let input = GetTransactionReceiptInput {
            transaction_hash: transaction_hash_bytes!(b"txn reverted"),
        };

        let receipt = get_transaction_receipt(context.clone(), input)
            .await
            .unwrap();

        let receipt = serde_json::to_value(receipt).unwrap();

        let expected = serde_json::json!({
            "transaction_hash": transaction_hash_bytes!(b"txn reverted"),
            "actual_fee": "0x0",
            "execution_status": "REVERTED",
            "finality_status": "ACCEPTED_ON_L2",
            "block_hash": block_hash_bytes!(b"latest"),
            "block_number": 2,
            "messages_sent": [],
            "revert_reason": "Reverted because",
            "events": [],
            "type": "INVOKE",
        });

        assert_eq!(receipt, expected);
    }
}
