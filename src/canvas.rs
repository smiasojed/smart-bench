use codec::Encode;
use color_eyre::eyre;
use sp_core::sr25519;
use sp_runtime::traits::{BlakeTwo256, Hash as _};
use subxt::PairSigner;

use super::*;

pub type Balance = u128;
pub type Gas = u64;
pub type AccountId = <api::DefaultConfig as subxt::Config>::AccountId;
pub type Hash = <api::DefaultConfig as subxt::Config>::Hash;
pub type Header = <api::DefaultConfig as subxt::Config>::Header;
pub type Signer = PairSigner<api::DefaultConfig, sr25519::Pair>;

#[subxt::subxt(runtime_metadata_path = "metadata/canvas.scale")]
pub mod api {}

async fn api() -> color_eyre::Result<api::RuntimeApi<api::DefaultConfig>> {
    Ok(subxt::ClientBuilder::new()
        // .set_url()
        .build()
        .await?
        .to_runtime_api::<api::RuntimeApi<api::DefaultConfig>>())
}

/// Submit extrinsic to instantiate a contract with the given code.
pub async fn instantiate_with_code<C: InkConstructor>(
    endowment: Balance,
    gas_limit: Gas,
    code: Vec<u8>,
    constructor: &C,
    salt: Vec<u8>,
    signer: &Signer,
) -> color_eyre::Result<AccountId> {
    let api = api().await?;

    let mut data = C::SELECTOR.to_vec();
    <C as Encode>::encode_to(&constructor, &mut data);

    let result = api
        .tx()
        .contracts()
        .instantiate_with_code(endowment, gas_limit, code, data, salt)
        .sign_and_submit_then_watch(signer)
        .await?;

    let instantiated = result
        .find_event::<api::contracts::events::Instantiated>()?
        .ok_or(eyre::eyre!("Failed to find Instantiated event"))?;

    Ok(instantiated.1)
}

/// Submit extrinsic to call a contract.
pub async fn call<M: InkMessage>(
    contract: AccountId,
    value: Balance,
    gas_limit: Gas,
    message: &M,
    signer: &Signer,
) -> color_eyre::Result<Hash> {
    let api = api().await?;

    let mut data = M::SELECTOR.to_vec();
    <M as Encode>::encode_to(&message, &mut data);

    let tx_hash = api
        .tx()
        .contracts()
        .call(contract.into(), value, gas_limit, data)
        .sign_and_submit(signer)
        .await?;

    Ok(tx_hash)
}

pub struct BlocksSubscription {
    task: async_std::task::JoinHandle<()>,
    receiver: std::sync::mpsc::Receiver<BlockExtrinsics>,
}

impl BlocksSubscription {
    pub async fn wait_for_txs(self, tx_hashes: &[Hash]) -> color_eyre::Result<ExtrinsicsResult> {
        let timeout = std::time::Duration::from_secs(30);
        let started = std::time::Instant::now();

        let mut blocks = Vec::new();
        let mut remaining_hashes: std::collections::HashSet<Hash> =
            tx_hashes.iter().cloned().collect();
        loop {
            if remaining_hashes.is_empty() {
                self.task.cancel().await;
                return Ok(ExtrinsicsResult { blocks });
            }

            if std::time::Instant::now() - started > timeout {
                return Err(eyre::eyre!(
                    "Timed out waiting for extrinsics. {} received",
                    tx_hashes.len() - remaining_hashes.len()
                ));
            }

            let block_xts = self.receiver.recv()?;
            for xt in &block_xts.extrinsics {
                remaining_hashes.remove(xt);
            }

            blocks.push(block_xts)
        }
    }
}

impl BlocksSubscription {
    pub async fn new() -> color_eyre::Result<Self> {
        let client: subxt::Client<api::DefaultConfig> = subxt::ClientBuilder::new().build().await?;
        let mut blocks_sub: jsonrpsee_types::Subscription<Header> =
            client.rpc().subscribe_blocks().await?;
        let (sender, receiver) = std::sync::mpsc::channel();

        let task = async_std::task::spawn(async move {
            while let Ok(Some(block_header)) = blocks_sub.next().await {
                if let Ok(Some(block)) = client.rpc().block(Some(block_header.hash())).await {
                    let extrinsics = block
                        .block
                        .extrinsics
                        .iter()
                        .map(|xt| BlakeTwo256::hash_of(xt))
                        .collect();
                    let block_extrinsics = BlockExtrinsics {
                        block_number: block_header.number,
                        block_hash: block_header.hash(),
                        extrinsics,
                    };
                    sender.send(block_extrinsics).expect("Receiver hung up");
                }
            }
        });

        Ok(Self { task, receiver })
    }
}

#[derive(Debug)]
pub struct ExtrinsicsResult {
    pub blocks: Vec<BlockExtrinsics>,
}

#[derive(Debug)]
pub struct BlockExtrinsics {
    pub block_number: u32,
    pub block_hash: Hash,
    pub extrinsics: Vec<Hash>,
}
