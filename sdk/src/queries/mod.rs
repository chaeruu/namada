//! Ledger read-only queries can be handled and dispatched via the [`RPC`]
//! defined via `router!` macro.

// Re-export to show in rustdoc!
use namada_core::ledger::storage::traits::StorageHasher;
use namada_core::ledger::storage::{DBIter, DB};
use namada_core::ledger::storage_api;
use namada_core::types::storage::BlockHeight;
pub use shell::Shell;
use shell::SHELL;
pub use types::{
    EncodedResponseQuery, Error, RequestCtx, RequestQuery, ResponseQuery,
    Router,
};
use vp::{Vp, VP};

pub use self::shell::eth_bridge::{
    Erc20FlowControl, GenBridgePoolProofReq, GenBridgePoolProofRsp,
    TransferToErcArgs,
};

#[macro_use]
mod router;
mod shell;
mod types;
pub mod vp;

// Most commonly expected patterns should be declared first
router! {RPC,
    // Shell provides storage read access, block metadata and can dry-run a tx
    ( "shell" ) = (sub SHELL),

    // Validity-predicate's specific storage queries
    ( "vp" ) = (sub VP),
}

/// Handle RPC query request in the ledger. On success, returns response with
/// borsh-encoded data.
pub fn handle_path<D, H, V, T>(
    ctx: RequestCtx<'_, D, H, V, T>,
    request: &RequestQuery,
) -> storage_api::Result<EncodedResponseQuery>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    RPC.handle(ctx, request)
}

// Handler helpers:

/// For queries that only support latest height, check that the given height is
/// not different from latest height, otherwise return an error.
pub fn require_latest_height<D, H, V, T>(
    ctx: &RequestCtx<'_, D, H, V, T>,
    request: &RequestQuery,
) -> storage_api::Result<()>
where
    D: 'static + DB + for<'iter> DBIter<'iter> + Sync,
    H: 'static + StorageHasher + Sync,
{
    if request.height.value() != 0
        && request.height.value()
            != ctx.wl_storage.storage.get_last_block_height().0
    {
        return Err(storage_api::Error::new_const(
            "This query doesn't support arbitrary block heights, only the \
             latest committed block height ('0' can be used as a special \
             value that means the latest block height)",
        ));
    }
    Ok(())
}

/// For queries that do not support proofs, check that proof is not requested,
/// otherwise return an error.
pub fn require_no_proof(request: &RequestQuery) -> storage_api::Result<()> {
    if request.prove {
        return Err(storage_api::Error::new_const(
            "This query doesn't support proofs",
        ));
    }
    Ok(())
}

/// For queries that don't use request data, require that there are no data
/// attached.
pub fn require_no_data(request: &RequestQuery) -> storage_api::Result<()> {
    if !request.data.is_empty() {
        return Err(storage_api::Error::new_const(
            "This query doesn't accept request data",
        ));
    }
    Ok(())
}

/// Queries testing helpers
#[cfg(any(test, feature = "testing"))]
mod testing {

    use namada_core::ledger::storage::testing::TestWlStorage;
    use namada_core::types::storage::BlockHeight;
    use tendermint_rpc::Response;

    use super::*;
    use crate::events::log::EventLog;
    use crate::tendermint_rpc::error::Error as RpcError;

    /// A test client that has direct access to the storage
    pub struct TestClient<RPC>
    where
        RPC: Router,
    {
        /// RPC router
        pub rpc: RPC,
        /// storage
        pub wl_storage: TestWlStorage,
        /// event log
        pub event_log: EventLog,
    }

    impl<RPC> TestClient<RPC>
    where
        RPC: Router,
    {
        #[allow(dead_code)]
        /// Initialize a test client for the given root RPC router
        pub fn new(rpc: RPC) -> Self {
            // Initialize the `TestClient`
            let mut wl_storage = TestWlStorage::default();

            // Initialize mock gas limit
            let max_block_gas_key =
                namada_core::ledger::parameters::storage::get_max_block_gas_key(
                );
            wl_storage
                .storage
                .write(
                    &max_block_gas_key,
                    namada_core::ledger::storage::types::encode(
                        &20_000_000_u64,
                    ),
                )
                .expect(
                    "Max block gas parameter must be initialized in storage",
                );
            let event_log = EventLog::default();
            Self {
                rpc,
                wl_storage,
                event_log,
            }
        }
    }

    #[cfg_attr(feature = "async-send", async_trait::async_trait)]
    #[cfg_attr(not(feature = "async-send"), async_trait::async_trait(?Send))]
    impl<RPC> Client for TestClient<RPC>
    where
        RPC: Router + Sync,
    {
        type Error = std::io::Error;

        async fn request(
            &self,
            path: String,
            data: Option<Vec<u8>>,
            height: Option<BlockHeight>,
            prove: bool,
        ) -> Result<EncodedResponseQuery, Self::Error> {
            let data = data.unwrap_or_default();
            let height = height.unwrap_or_default();
            let height: crate::tendermint::block::Height =
                height.try_into().map_err(|err| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, err)
                })?;
            // Handle a path by invoking the `RPC.handle` directly with the
            // borrowed storage
            let request = RequestQuery {
                data: data.into(),
                path,
                height,
                prove,
            };
            let ctx = RequestCtx {
                wl_storage: &self.wl_storage,
                event_log: &self.event_log,
                vp_wasm_cache: (),
                tx_wasm_cache: (),
                storage_read_past_height_limit: None,
            };
            // TODO: this is a hack to propagate errors to the caller, we should
            // really permit error types other than [`std::io::Error`]
            self.rpc.handle(ctx, &request).map_err(|err| {
                std::io::Error::new(std::io::ErrorKind::Other, err.to_string())
            })
        }

        async fn perform<R>(&self, _request: R) -> Result<R::Output, RpcError>
        where
            R: tendermint_rpc::SimpleRequest,
        {
            Ok(R::Response::from_string("TODO").unwrap().into())
        }
    }
}

use std::fmt::{Debug, Display};

use tendermint_rpc::endpoint::{
    abci_info, block, block_results, blockchain, commit, consensus_params,
    consensus_state, health, net_info, status,
};
use tendermint_rpc::query::Query;
use tendermint_rpc::{Error as RpcError, Order};

use crate::tendermint::abci::response::Info;
use crate::tendermint::block::Height;

/// A client with async request dispatcher method, which can be used to invoke
/// type-safe methods from a root [`crate::ledger::queries::Router`], generated
/// via `router!` macro.
#[cfg(any(test, feature = "async-client"))]
#[cfg_attr(feature = "async-send", async_trait::async_trait)]
#[cfg_attr(not(feature = "async-send"), async_trait::async_trait(?Send))]
pub trait Client {
    /// `std::io::Error` can happen in decoding with
    /// `BorshDeserialize::try_from_slice`
    type Error: From<std::io::Error> + Display + Debug;

    /// Send a simple query request at the given path. For more options, use the
    /// `request` method.
    async fn simple_request(
        &self,
        path: String,
    ) -> Result<Vec<u8>, Self::Error> {
        self.request(path, None, None, false)
            .await
            .map(|response| response.data)
    }

    /// Send a query request at the given path.
    async fn request(
        &self,
        path: String,
        data: Option<Vec<u8>>,
        height: Option<BlockHeight>,
        prove: bool,
    ) -> Result<EncodedResponseQuery, Self::Error>;

    /// `/abci_info`: get information about the ABCI application.
    async fn abci_info(&self) -> Result<Info, RpcError> {
        Ok(self.perform(abci_info::Request).await?.response)
    }

    /// `/broadcast_tx_sync`: broadcast a transaction, returning the response
    /// from `CheckTx`.
    async fn broadcast_tx_sync(
        &self,
        tx: impl Into<Vec<u8>>,
    ) -> Result<tendermint_rpc::endpoint::broadcast::tx_sync::Response, RpcError>
    {
        self.perform(
            tendermint_rpc::endpoint::broadcast::tx_sync::Request::new(tx),
        )
        .await
    }

    /// `/block`: get the latest block.
    async fn latest_block(&self) -> Result<block::Response, RpcError> {
        self.perform(block::Request::default()).await
    }

    /// `/block`: get block at a given height.
    async fn block<H>(&self, height: H) -> Result<block::Response, RpcError>
    where
        H: Into<Height> + Send,
    {
        self.perform(block::Request::new(height.into())).await
    }

    /// `/block_search`: search for blocks by BeginBlock and EndBlock events.
    async fn block_search(
        &self,
        query: Query,
        page: u32,
        per_page: u8,
        order: Order,
    ) -> Result<tendermint_rpc::endpoint::block_search::Response, RpcError>
    {
        self.perform(tendermint_rpc::endpoint::block_search::Request::new(
            query, page, per_page, order,
        ))
        .await
    }

    /// `/block_results`: get ABCI results for a block at a particular height.
    async fn block_results<H>(
        &self,
        height: H,
    ) -> Result<tendermint_rpc::endpoint::block_results::Response, RpcError>
    where
        H: Into<Height> + Send,
    {
        self.perform(tendermint_rpc::endpoint::block_results::Request::new(
            height.into(),
        ))
        .await
    }

    /// `/tx_search`: search for transactions with their results.
    async fn tx_search(
        &self,
        query: Query,
        prove: bool,
        page: u32,
        per_page: u8,
        order: Order,
    ) -> Result<tendermint_rpc::endpoint::tx_search::Response, RpcError> {
        self.perform(tendermint_rpc::endpoint::tx_search::Request::new(
            query, prove, page, per_page, order,
        ))
        .await
    }

    /// `/abci_query`: query the ABCI application
    async fn abci_query<V>(
        &self,
        path: Option<String>,
        data: V,
        height: Option<Height>,
        prove: bool,
    ) -> Result<tendermint_rpc::endpoint::abci_query::AbciQuery, RpcError>
    where
        V: Into<Vec<u8>> + Send,
    {
        Ok(self
            .perform(tendermint_rpc::endpoint::abci_query::Request::new(
                path, data, height, prove,
            ))
            .await?
            .response)
    }

    /// `/block_results`: get ABCI results for the latest block.
    async fn latest_block_results(
        &self,
    ) -> Result<block_results::Response, RpcError> {
        self.perform(block_results::Request::default()).await
    }

    /// `/blockchain`: get block headers for `min` <= `height` <= `max`.
    ///
    /// Block headers are returned in descending order (highest first).
    ///
    /// Returns at most 20 items.
    async fn blockchain<H>(
        &self,
        min: H,
        max: H,
    ) -> Result<blockchain::Response, RpcError>
    where
        H: Into<Height> + Send,
    {
        // TODO(tarcieri): return errors for invalid params before making
        // request?
        self.perform(blockchain::Request::new(min.into(), max.into()))
            .await
    }

    /// `/commit`: get block commit at a given height.
    async fn commit<H>(&self, height: H) -> Result<commit::Response, RpcError>
    where
        H: Into<Height> + Send,
    {
        self.perform(commit::Request::new(height.into())).await
    }

    /// `/consensus_params`: get current consensus parameters at the specified
    /// height.
    async fn consensus_params<H>(
        &self,
        height: H,
    ) -> Result<consensus_params::Response, RpcError>
    where
        H: Into<Height> + Send,
    {
        self.perform(consensus_params::Request::new(Some(height.into())))
            .await
    }

    /// `/consensus_state`: get current consensus state
    async fn consensus_state(
        &self,
    ) -> Result<consensus_state::Response, RpcError> {
        self.perform(consensus_state::Request::new()).await
    }

    /// `/consensus_params`: get the latest consensus parameters.
    async fn latest_consensus_params(
        &self,
    ) -> Result<consensus_params::Response, RpcError> {
        self.perform(consensus_params::Request::new(None)).await
    }

    /// `/commit`: get the latest block commit
    async fn latest_commit(&self) -> Result<commit::Response, RpcError> {
        self.perform(commit::Request::default()).await
    }

    /// `/health`: get node health.
    ///
    /// Returns empty result (200 OK) on success, no response in case of an
    /// error.
    async fn health(&self) -> Result<(), RpcError> {
        self.perform(health::Request).await?;
        Ok(())
    }

    /// `/net_info`: obtain information about P2P and other network connections.
    async fn net_info(&self) -> Result<net_info::Response, RpcError> {
        self.perform(net_info::Request).await
    }

    /// `/status`: get Tendermint status including node info, pubkey, latest
    /// block hash, app hash, block height and time.
    async fn status(&self) -> Result<status::Response, RpcError> {
        self.perform(status::Request).await
    }

    /// Perform a request against the RPC endpoint
    async fn perform<R>(&self, request: R) -> Result<R::Output, RpcError>
    where
        R: tendermint_rpc::SimpleRequest;
}

#[cfg_attr(feature = "async-send", async_trait::async_trait)]
#[cfg_attr(not(feature = "async-send"), async_trait::async_trait(?Send))]
impl<C: tendermint_rpc::client::Client + std::marker::Sync> Client for C {
    type Error = Error;

    async fn request(
        &self,
        path: String,
        data: Option<Vec<u8>>,
        height: Option<BlockHeight>,
        prove: bool,
    ) -> Result<EncodedResponseQuery, Self::Error> {
        let data = data.unwrap_or_default();
        let height = height
            .map(|height| {
                crate::tendermint::block::Height::try_from(height.0)
                    .map_err(|_err| Error::InvalidHeight(height))
            })
            .transpose()?;
        let response = self
            .abci_query(
                // TODO open the private Path constructor in tendermint-rpc
                Some(std::str::FromStr::from_str(&path).unwrap()),
                data,
                height,
                prove,
            )
            .await?;
        use crate::tendermint::abci::Code;
        match response.code {
            Code::Ok => Ok(EncodedResponseQuery {
                data: response.value,
                info: response.info,
                proof: response.proof,
            }),
            Code::Err(code) => Err(Error::Query(response.info, code.into())),
        }
    }

    async fn perform<R>(&self, request: R) -> Result<R::Output, RpcError>
    where
        R: tendermint_rpc::SimpleRequest,
    {
        tendermint_rpc::client::Client::perform(self, request).await
    }
}
