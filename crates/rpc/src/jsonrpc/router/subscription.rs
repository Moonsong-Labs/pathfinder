use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use pathfinder_common::{BlockId, BlockNumber};
use serde_json::value::RawValue;
use tokio::sync::{mpsc, RwLock};
use tracing::Instrument;

use super::{run_concurrently, RpcRouter};
use crate::context::RpcContext;
use crate::dto::serialize::SerializeForVersion;
use crate::dto::DeserializeForVersion;
use crate::error::ApplicationError;
use crate::jsonrpc::{RequestId, RpcError, RpcRequest, RpcResponse};
use crate::{RpcVersion, SubscriptionId};

/// See [`RpcSubscriptionFlow`].
#[axum::async_trait]
pub(super) trait RpcSubscriptionEndpoint: Send + Sync {
    // Start the subscription.
    async fn invoke(&self, params: InvokeParams) -> Result<tokio::task::JoinHandle<()>, RpcError>;
}

pub(super) struct InvokeParams {
    router: RpcRouter,
    input: serde_json::Value,
    subscription_id: SubscriptionId,
    subscriptions: Arc<DashMap<SubscriptionId, tokio::task::JoinHandle<()>>>,
    req_id: RequestId,
    ws_tx: mpsc::Sender<Result<Message, RpcResponse>>,
    lock: Arc<RwLock<()>>,
}

/// This trait is the main entry point for subscription endpoint
/// implementations.
///
/// Many subscription endpoints allow for historical data to be streamed before
/// starting to stream active updates. This is done by having the subscription
/// request pass a `block` parameter indicating the block to start from. This
/// trait is designed to make it easy to implement this behavior, and difficult
/// to make mistakes (e.g. race conditions or accidentally dropping messages).
///
/// The `catch_up` method is used to stream historical data, while the
/// `subscribe` method is used to subscribe to active updates. The
/// `starting_block` method extracts the first block to start streaming from.
/// This will probably always just be the `block` field of the request.
///
/// If a subscription endpoint does not need to stream historical data, it
/// should always return an empty vec from `catch_up`.
///
/// The flow is implemented as follows:
/// - Catch up from the starting block to the latest block known to pathfinder,
///   in batches. Call that block K.
/// - Subscribe to active updates. Fetch the first update, along with the block
///   number that it applies to.
/// - Catch up from block K to the block just before the first active update.
///   This is done to ensure that no blocks are missed between the previous
///   catch-up and the subscription.
/// - Stream the first active update, and then keep streaming the rest.
#[axum::async_trait]
pub trait RpcSubscriptionFlow: Send + Sync {
    /// `params` field of the subscription request.
    type Params: crate::dto::DeserializeForVersion + Clone + Send + Sync + 'static;
    /// The notification type to be sent to the client.
    type Notification: crate::dto::serialize::SerializeForVersion + Send + Sync + 'static;

    /// The block to start streaming from. If the subscription endpoint does not
    /// support catching up, this method should always return
    /// [`BlockId::Latest`].
    fn starting_block(params: &Self::Params) -> BlockId;

    /// Fetch historical data from the `from` block to the `to` block. The
    /// range is inclusive on both ends. If there is no historical data in the
    /// range, return an empty vec.
    async fn catch_up(
        state: &RpcContext,
        params: &Self::Params,
        from: BlockNumber,
        to: BlockNumber,
    ) -> Result<Vec<SubscriptionMessage<Self::Notification>>, RpcError>;

    /// Subscribe to active updates.
    async fn subscribe(
        state: RpcContext,
        params: Self::Params,
        tx: mpsc::Sender<SubscriptionMessage<Self::Notification>>,
    );
}

#[derive(Debug)]
pub struct SubscriptionMessage<T> {
    /// [`RpcSubscriptionFlow::Notification`] to be sent to the client.
    pub notification: T,
    /// The block number of the notification. If the notification does not have
    /// a block number, this value does not matter.
    pub block_number: BlockNumber,
    /// The value for the `method` field of the subscription notification sent
    /// to the client.
    pub subscription_name: &'static str,
}

#[axum::async_trait]
impl<T> RpcSubscriptionEndpoint for T
where
    T: RpcSubscriptionFlow + 'static,
{
    async fn invoke(
        &self,
        InvokeParams {
            router,
            input,
            subscription_id,
            subscriptions,
            req_id,
            ws_tx,
            lock,
        }: InvokeParams,
    ) -> Result<tokio::task::JoinHandle<()>, RpcError> {
        let req = T::Params::deserialize(crate::dto::Value::new(input, router.version))
            .map_err(|e| RpcError::InvalidParams(e.to_string()))?;
        let tx = SubscriptionSender {
            subscription_id,
            subscriptions,
            tx: ws_tx.clone(),
            version: router.version,
            _phantom: Default::default(),
        };

        let first_block = T::starting_block(&req);

        let mut current_block = match &first_block {
            BlockId::Pending => {
                return Err(RpcError::InvalidParams(
                    "Pending block not supported".to_string(),
                ));
            }
            BlockId::Latest => {
                // No need to catch up. The code below will subscribe to new blocks.
                BlockNumber::MAX
            }
            BlockId::Number(_) | BlockId::Hash(_) => {
                // Load the first block number, return an error if it's invalid.
                let first_block = pathfinder_storage::BlockId::try_from(first_block)
                    .map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let storage = router.context.storage.clone();
                tokio::task::spawn_blocking(move || -> Result<_, RpcError> {
                    let mut conn = storage.connection().map_err(RpcError::InternalError)?;
                    let db = conn.transaction().map_err(RpcError::InternalError)?;
                    db.block_number(first_block)
                        .map_err(RpcError::InternalError)?
                        .ok_or_else(|| ApplicationError::BlockNotFound.into())
                })
                .await
                .map_err(|e| RpcError::InternalError(e.into()))??
            }
        };

        Ok(tokio::spawn(async move {
            // This lock ensures that the streaming of subscriptions doesn't start before
            // the caller sends the success response for the subscription request.
            let _guard = lock.read().await;

            // Catch up to the latest block in batches of BATCH_SIZE.
            const BATCH_SIZE: u64 = 64;
            loop {
                let messages = match T::catch_up(
                    &router.context,
                    &req,
                    current_block,
                    current_block + BATCH_SIZE,
                )
                .await
                {
                    Ok(messages) => messages,
                    Err(e) => {
                        tx.send_err(e, req_id.clone())
                            .await
                            // Could error if the subscription is closing.
                            .ok();
                        return;
                    }
                };
                if messages.is_empty() {
                    // Caught up.
                    break;
                }
                for msg in messages {
                    if tx
                        .send(msg.notification, msg.subscription_name)
                        .await
                        .is_err()
                    {
                        // Subscription closing.
                        return;
                    }
                    current_block = msg.block_number;
                }
                // Increment the current block by 1 because the catch_up range is inclusive.
                current_block += 1;
            }

            // Subscribe to new blocks. Receive the first subscription message.
            let (tx1, mut rx1) = mpsc::channel::<SubscriptionMessage<T::Notification>>(1024);
            {
                let req = req.clone();
                tokio::spawn(T::subscribe(router.context.clone(), req, tx1));
            }
            let first_msg = match rx1.recv().await {
                Some(msg) => msg,
                None => {
                    // Subscription closing.
                    return;
                }
            };

            // Catch up from the latest block that we already caught up to, to the first
            // block that will be streamed from the subscription. This way we don't miss any
            // blocks. Because the catch_up range is inclusive, we need to subtract 1 from
            // the block number.
            if let Some(block_number) = first_msg.block_number.parent() {
                let messages =
                    match T::catch_up(&router.context, &req, current_block, block_number).await {
                        Ok(messages) => messages,
                        Err(e) => {
                            tx.send_err(e, req_id.clone())
                                .await
                                // Could error if the subscription is closing.
                                .ok();
                            return;
                        }
                    };
                for msg in messages {
                    if tx
                        .send(msg.notification, msg.subscription_name)
                        .await
                        .is_err()
                    {
                        // Subscription closing.
                        return;
                    }
                }
            }

            // Send the first subscription message and then forward the rest.
            if tx
                .send(first_msg.notification, first_msg.subscription_name)
                .await
                .is_err()
            {
                // Subscription closing.
                return;
            }
            let mut last_block = first_msg.block_number;
            while let Some(msg) = rx1.recv().await {
                if msg.block_number.get() > last_block.get() + 1 {
                    // One or more blocks have been skipped. This is likely due to a race
                    // condition resulting from a reorg. This message should be ignored.
                    continue;
                }
                if tx
                    .send(msg.notification, msg.subscription_name)
                    .await
                    .is_err()
                {
                    // Subscription closing.
                    break;
                }
                last_block = msg.block_number;
            }
        }))
    }
}

type WsSender = mpsc::Sender<Result<Message, RpcResponse>>;
type WsReceiver = mpsc::Receiver<Result<Message, axum::Error>>;

/// Split a websocket into an MPSC sender and receiver.
/// These two are later passed to [`handle_json_rpc_socket`]. This separation
/// serves to allow easier testing. The sender sends `Result<_, RpcResponse>`
/// purely for convenience, and the [`RpcResponse`] will be encoded into a
/// [`Message::Text`].
pub fn split_ws(ws: WebSocket) -> (WsSender, WsReceiver) {
    let (mut ws_sender, mut ws_receiver) = ws.split();
    // Send messages to the websocket using an MPSC channel.
    let (sender_tx, mut sender_rx) = mpsc::channel::<Result<Message, RpcResponse>>(1024);
    tokio::spawn(async move {
        while let Some(msg) = sender_rx.recv().await {
            match msg {
                Ok(msg) => {
                    if ws_sender.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    if ws_sender
                        .send(Message::Text(serde_json::to_string(&e).unwrap()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    // Receive messages from the websocket using an MPSC channel.
    let (receiver_tx, receiver_rx) = mpsc::channel::<Result<Message, axum::Error>>(1024);
    tokio::spawn(async move {
        while let Some(msg) = ws_receiver.next().await {
            if receiver_tx.send(msg).await.is_err() {
                break;
            }
        }
    });
    (sender_tx, receiver_rx)
}

pub fn handle_json_rpc_socket(
    state: RpcRouter,
    ws_tx: mpsc::Sender<Result<Message, RpcResponse>>,
    mut ws_rx: mpsc::Receiver<Result<Message, axum::Error>>,
) {
    let subscriptions: Arc<DashMap<SubscriptionId, tokio::task::JoinHandle<()>>> =
        Default::default();
    // Read and handle messages from the websocket.
    tokio::spawn(async move {
        loop {
            let request = match ws_rx.recv().await {
                Some(Ok(Message::Text(msg))) => msg,
                Some(Ok(Message::Binary(bytes))) => match String::from_utf8(bytes) {
                    Ok(msg) => msg,
                    Err(e) => {
                        if ws_tx
                            .send(Err(RpcResponse::parse_error(e.to_string())))
                            .await
                            .is_err()
                        {
                            // Connection is closing.
                            break;
                        }
                        continue;
                    }
                },
                Some(Ok(Message::Pong(_) | Message::Ping(_))) => {
                    // Ping and pong messages are handled automatically by axum.
                    continue;
                }
                Some(Ok(Message::Close(_))) | None => {
                    // Websocket closed.
                    return;
                }
                Some(Err(e)) => {
                    tracing::trace!(error = ?e, "Error receiving websocket message");
                    return;
                }
            };

            // This lock ensures that the streaming of subscriptions doesn't start before we
            // send the success response for the subscription request. Once this write guard
            // is dropped, all of the read guards can proceed.
            let lock = Arc::new(RwLock::new(()));
            let _guard = lock.write().await;

            // Unfortunately due to this https://github.com/serde-rs/json/issues/497
            // we cannot use an enum with borrowed raw values inside to do a single
            // deserialization for us. Instead we have to distinguish manually
            // between a single request and a batch request which we do by checking
            // the first byte.
            let request = request.trim_start();
            if !request.starts_with('[') {
                let raw_value: &RawValue = match serde_json::from_str(request) {
                    Ok(raw_value) => raw_value,
                    Err(e) => {
                        if ws_tx
                            .send(Err(RpcResponse::parse_error(e.to_string())))
                            .await
                            .is_err()
                        {
                            // Connection is closing.
                            break;
                        }
                        continue;
                    }
                };
                match handle_request(
                    &state,
                    raw_value,
                    subscriptions.clone(),
                    ws_tx.clone(),
                    lock.clone(),
                )
                .await
                {
                    Ok(Some(response)) | Err(response) => {
                        if ws_tx
                            .send(Ok(Message::Text(serde_json::to_string(&response).unwrap())))
                            .await
                            .is_err()
                        {
                            // Connection is closing.
                            break;
                        }
                    }
                    Ok(None) => {
                        // No response.
                        continue;
                    }
                }
            } else {
                // Batch request.
                let requests = match serde_json::from_str::<Vec<&RawValue>>(request) {
                    Ok(requests) => requests,
                    Err(e) => {
                        if ws_tx
                            .send(Err(RpcResponse::parse_error(e.to_string())))
                            .await
                            .is_err()
                        {
                            // Connection is closing.
                            break;
                        }
                        continue;
                    }
                };

                if requests.is_empty() {
                    // According to the JSON-RPC spec, a batch request cannot be empty.
                    if ws_tx
                        .send(Err(RpcResponse::invalid_request(
                            "A batch request must contain at least one request".to_owned(),
                        )))
                        .await
                        .is_err()
                    {
                        // Connection is closing.
                        break;
                    }
                }

                let responses = run_concurrently(
                    state.context.config.batch_concurrency_limit,
                    requests.into_iter().enumerate(),
                    {
                        |(idx, request)| {
                            let state = &state;
                            let ws_tx = ws_tx.clone();
                            let subscriptions = subscriptions.clone();
                            let lock = lock.clone();
                            async move {
                                match handle_request(state, request, subscriptions, ws_tx, lock)
                                    .instrument(tracing::debug_span!("ws batch", idx))
                                    .await
                                {
                                    Ok(Some(response)) | Err(response) => Some(response),
                                    Ok(None) => None,
                                }
                            }
                        }
                    },
                )
                .await
                .flatten()
                .collect::<Vec<RpcResponse>>();

                // All requests were notifications, no response needed.
                if responses.is_empty() {
                    continue;
                }

                if ws_tx
                    .send(Ok(Message::Text(
                        serde_json::to_string(&responses).unwrap(),
                    )))
                    .await
                    .is_err()
                {
                    // Connection is closing.
                    break;
                }
            }
        }
    });
}

/// Handle a single request. Returns `Result` for convenience, so that the `?`
/// operator could be used in the body of the function. Returns `Ok(None)` if
/// the request was a notification (i.e. no response is needed).
async fn handle_request(
    state: &RpcRouter,
    raw_request: &RawValue,
    subscriptions: Arc<DashMap<SubscriptionId, tokio::task::JoinHandle<()>>>,
    ws_tx: mpsc::Sender<Result<Message, RpcResponse>>,
    lock: Arc<RwLock<()>>,
) -> Result<Option<RpcResponse>, RpcResponse> {
    let rpc_request = serde_json::from_str::<RpcRequest<'_>>(raw_request.get())
        .map_err(|e| RpcResponse::invalid_request(e.to_string()))?;
    let req_id = rpc_request.id;

    // Ignore notification requests.
    if req_id.is_notification() {
        return Ok(None);
    }

    // Handle JSON-RPC non-subscription methods.
    if state
        .method_endpoints
        .contains_key(rpc_request.method.as_ref())
    {
        return Ok(state.run_request(raw_request.get()).await);
    }

    // Handle starknet_unsubscribe.
    if rpc_request.method == "starknet_unsubscribe" {
        // End the subscription.
        let params = rpc_request.params.0.ok_or_else(|| {
            RpcResponse::invalid_params(
                req_id.clone(),
                "Missing params for starknet_unsubscribe".to_string(),
            )
        })?;
        let params = serde_json::from_str::<StarknetUnsubscribeParams>(params.get())
            .map_err(|e| RpcResponse::invalid_params(req_id.clone(), e.to_string()))?;
        let (_, handle) = subscriptions
            .remove(&params.subscription_id)
            .ok_or_else(|| {
                RpcResponse::invalid_params(req_id.clone(), "Subscription not found".to_string())
            })?;
        handle.abort();
        metrics::increment_counter!("rpc_method_calls_total", "method" => "starknet_unsubscribe", "version" => state.version.to_str());
        return Ok(Some(RpcResponse {
            output: Ok(true.into()),
            id: req_id,
        }));
    }

    let (&method_name, endpoint) = state
        .subscription_endpoints
        .get_key_value(rpc_request.method.as_ref())
        .ok_or_else(|| RpcResponse::method_not_found(req_id.clone()))?;
    metrics::increment_counter!("rpc_method_calls_total", "method" => method_name, "version" => state.version.to_str());

    let params = serde_json::to_value(rpc_request.params)
        .map_err(|e| RpcResponse::invalid_params(req_id.clone(), e.to_string()))?;

    // Start the subscription.
    let state = state.clone();
    let subscription_id = SubscriptionId::next();
    let ws_tx = ws_tx.clone();
    match endpoint
        .invoke(InvokeParams {
            router: state,
            input: params,
            subscription_id,
            subscriptions: subscriptions.clone(),
            req_id: req_id.clone(),
            ws_tx: ws_tx.clone(),
            lock,
        })
        .await
    {
        Ok(handle) => {
            if subscriptions.insert(subscription_id, handle).is_some() {
                panic!("subscription id overflow");
            }
            Ok(Some(RpcResponse {
                output: Ok(
                    serde_json::to_value(&SubscriptionIdResult { subscription_id }).unwrap(),
                ),
                id: req_id,
            }))
        }
        Err(e) => Err(RpcResponse {
            output: Err(e),
            id: req_id,
        }),
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StarknetUnsubscribeParams {
    subscription_id: SubscriptionId,
}

#[derive(Debug, serde::Serialize)]
struct SubscriptionIdResult {
    subscription_id: SubscriptionId,
}

#[derive(Debug)]
pub struct SubscriptionSender<T> {
    pub subscription_id: SubscriptionId,
    pub subscriptions: Arc<DashMap<SubscriptionId, tokio::task::JoinHandle<()>>>,
    pub tx: mpsc::Sender<Result<Message, RpcResponse>>,
    pub version: RpcVersion,
    pub _phantom: std::marker::PhantomData<T>,
}

impl<T> Clone for SubscriptionSender<T> {
    fn clone(&self) -> Self {
        Self {
            subscription_id: self.subscription_id,
            subscriptions: self.subscriptions.clone(),
            tx: self.tx.clone(),
            version: self.version,
            _phantom: Default::default(),
        }
    }
}

impl<T: crate::dto::serialize::SerializeForVersion> SubscriptionSender<T> {
    pub async fn send(
        &self,
        value: T,
        subscription_name: &'static str,
    ) -> Result<(), mpsc::error::SendError<()>> {
        if !self.subscriptions.contains_key(&self.subscription_id) {
            // Race condition due to the subscription ending.
            return Ok(());
        }
        let notification = RpcNotification {
            jsonrpc: "2.0",
            method: subscription_name,
            params: SubscriptionResult {
                subscription_id: self.subscription_id,
                result: value,
            },
        }
        .serialize(crate::dto::serialize::Serializer::new(self.version))
        .unwrap();
        let data = serde_json::to_string(&notification).unwrap();
        self.tx
            .send(Ok(Message::Text(data)))
            .await
            .map_err(|_| mpsc::error::SendError(()))
    }

    pub async fn send_err(
        &self,
        err: RpcError,
        req_id: RequestId,
    ) -> Result<(), mpsc::error::SendError<()>> {
        self.tx
            .send(Err(RpcResponse {
                output: Err(err),
                id: req_id,
            }))
            .await
            .map_err(|_| mpsc::error::SendError(()))
    }
}

#[derive(Debug)]
struct RpcNotification<T> {
    jsonrpc: &'static str,
    method: &'static str,
    params: SubscriptionResult<T>,
}

#[derive(Debug)]
pub struct SubscriptionResult<T> {
    subscription_id: SubscriptionId,
    result: T,
}

impl<T> crate::dto::serialize::SerializeForVersion for RpcNotification<T>
where
    T: crate::dto::serialize::SerializeForVersion,
{
    fn serialize(
        &self,
        serializer: crate::dto::serialize::Serializer,
    ) -> Result<crate::dto::serialize::Ok, crate::dto::serialize::Error> {
        let mut serializer = serializer.serialize_struct()?;
        serializer.serialize_field("jsonrpc", &self.jsonrpc)?;
        serializer.serialize_field("method", &self.method)?;
        serializer.serialize_field("params", &self.params)?;
        serializer.end()
    }
}

impl<T> crate::dto::serialize::SerializeForVersion for SubscriptionResult<T>
where
    T: crate::dto::serialize::SerializeForVersion,
{
    fn serialize(
        &self,
        serializer: crate::dto::serialize::Serializer,
    ) -> Result<crate::dto::serialize::Ok, crate::dto::serialize::Error> {
        let mut serializer = serializer.serialize_struct()?;
        serializer.serialize_field("subscription_id", &self.subscription_id)?;
        serializer.serialize_field("result", &self.result)?;
        serializer.end()
    }
}