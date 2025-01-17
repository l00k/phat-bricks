#![cfg_attr(not(feature = "std"), no_std, no_main)]

extern crate alloc;

pub use crate::action_offchain_rollup::*;

#[ink::contract(env = pink_extension::PinkEnvironment)]
mod action_offchain_rollup {
    use alloc::{string::String, vec::Vec};
    use ink::env::call::{build_call, ExecutionInput, Selector};
    #[cfg(feature = "std")]
    use ink::storage::traits::StorageLayout;
    use ink::storage::Lazy;
    use pink_extension as pink;
    use pink_extension::chain_extension::signing;
    use pink_web3::{
        api::{Eth, Namespace},
        keys::pink::KeyPair,
        signing::Key,
        transports::{resolve_ready, PinkHttp},
        types::H160,
    };
    use scale::{Decode, Encode};
    use this_crate::{version_tuple, VersionTuple};

    // To enable `(result).log_err("Reason")?`
    use logging::ResultExt;

    use ethabi::Token;
    use logging::error;
    use phat_js as js;
    use phat_offchain_rollup::{
        clients::evm::{sign_meta_tx, EvmRollupClient},
        Action,
    };

    type CodeHash = [u8; 32];

    #[derive(Clone, Encode, Decode)]
    #[cfg_attr(feature = "std", derive(scale_info::TypeInfo, StorageLayout))]
    pub struct Core {
        /// The JS code that processes the rollup queue request
        script: String,
        /// The configuration that would be passed to the core js script
        settings: String,
        /// The code hash of the core js script
        code_hash: CodeHash,
    }

    #[ink(storage)]
    pub struct ActionOffchainRollup {
        owner: AccountId,
        /// Key for signing the rollup tx
        attest_key: [u8; 32],
        /// BrickProfile address to ask for tx signing (to pay gas fee)
        brick_profile: AccountId,
        client: Option<Client>,
        /// The JS code that processes the rollup queue request
        core: Lazy<Core>,
    }

    #[derive(Clone, Encode, Decode, Debug)]
    #[cfg_attr(feature = "std", derive(scale_info::TypeInfo, StorageLayout))]
    pub struct Client {
        /// The RPC endpoint of the target blockchain
        rpc: String,
        /// The client smart contract address on the target blockchain
        client_addr: [u8; 20],
    }

    #[derive(Clone, Encode, Decode, Debug)]
    #[cfg_attr(feature = "std", derive(scale_info::TypeInfo, StorageLayout))]
    pub struct Configuration {
        rpc: Option<String>,
        client_addr: Option<[u8; 20]>,
        script: Option<String>,
        settings: Option<String>,
        code_hash: Option<CodeHash>,
    }

    #[derive(Encode, Decode, Debug, PartialEq)]
    #[repr(u8)]
    #[cfg_attr(feature = "std", derive(scale_info::TypeInfo))]
    pub enum Error {
        BadOrigin,
        ClientNotConfigured,
        CoreNotConfigured,
        BadBrickProfile,

        InvalidAddressLength,
        NoRequestInQueue,
        FailedToCreateClient,
        FailedToCommitTx,

        FailedToGetStorage,
        FailedToCreateTransaction,
        FailedToSignTransaction,
        FailedToSendTransaction,

        InvalidJsOutput,
        JsError(String),
    }

    type Result<T> = core::result::Result<T, Error>;

    impl ActionOffchainRollup {
        #[ink(constructor)]
        pub fn new(brick_profile: AccountId) -> Self {
            const NONCE: &[u8] = b"attest_key";
            let random = signing::derive_sr25519_key(NONCE);
            Self {
                owner: Self::env().caller(),
                attest_key: random[..32]
                    .try_into()
                    .expect("random is long enough; qed."),
                brick_profile,
                client: None,
                core: Default::default(),
            }
        }

        /// @ui core_js widget codemirror
        /// @ui core_js options.lang javascript
        #[ink(constructor)]
        pub fn with_core(core_js: String, core_settings: String, brick_profile: AccountId) -> Self {
            let mut this = Self::new(brick_profile);
            this.config_core_inner(core_js, core_settings);
            this
        }

        /// @ui core_js widget codemirror
        /// @ui core_js options.lang javascript
        #[ink(constructor)]
        pub fn with_configuration(
            client_rpc: String,
            client_addr: Vec<u8>,
            core_js: String,
            core_settings: String,
            brick_profile: AccountId,
        ) -> Self {
            let mut this = Self::new(brick_profile);
            this.config_core_inner(core_js, core_settings);
            this.config_client(client_rpc, client_addr)
                .expect("failed to configure client");
            this
        }

        /// @category Metadata
        #[ink(message)]
        pub fn version(&self) -> VersionTuple {
            version_tuple!()
        }

        /// Gets the owner of the contract.
        ///
        /// @category Metadata
        ///
        #[ink(message)]
        pub fn owner(&self) -> AccountId {
            self.owner
        }

        /// Get the identity of offchain rollup.
        ///
        /// @category Configuration
        ///
        #[ink(message)]
        pub fn get_attest_address(&self) -> H160 {
            KeyPair::from(self.attest_key).address()
        }

        ///
        /// @category Metadata
        ///
        #[ink(message)]
        pub fn get_brick_profile_address(&self) -> AccountId {
            self.brick_profile
        }

        /// Set the BrickProfile address (only owner).
        ///
        /// @category Metadata
        ///
        #[ink(message)]
        pub fn set_brick_profile_address(&mut self, brick_profile: AccountId) -> Result<()> {
            self.ensure_owner()?;
            self.brick_profile = brick_profile;
            Ok(())
        }

        /// Get client contract address and RPC endpoint (only owner).
        ///
        /// @category Configuration
        ///
        #[ink(message)]
        pub fn get_client(&self) -> Result<Client> {
            self.ensure_owner()?;
            let client = self.ensure_client_configured()?;
            Ok(client.clone())
        }

        /// Get script and settings (only owner).
        ///
        /// @category Configuration
        ///
        #[ink(message)]
        pub fn get_core(&self) -> Result<Option<Core>> {
            self.ensure_owner()?;
            Ok(self.core.get())
        }

        /// Configures the core script (only owner).
        ///
        /// @category Configuration
        ///
        /// @ui core_js widget codemirror
        /// @ui core_js options.lang javascript
        ///
        #[ink(message)]
        pub fn config_core(&mut self, core_js: String, settings: String) -> Result<()> {
            self.ensure_owner()?;
            self.config_core_inner(core_js, settings);
            Ok(())
        }

        /// Set the core script (only owner).
        ///
        /// @category Configuration
        ///
        /// @ui core_js widget codemirror
        /// @ui core_js options.lang javascript
        ///
        #[ink(message)]
        pub fn config_core_script(&mut self, core_js: String) -> Result<()> {
            self.ensure_owner()?;
            let Some(core) = self.core.get() else {
                return Err(Error::CoreNotConfigured);
            };
            self.config_core_inner(core_js, core.settings);
            Ok(())
        }

        /// Set the configuration (only owner).
        ///
        /// @category Configuration
        ///
        #[ink(message)]
        pub fn config_core_settings(&mut self, settings: String) -> Result<()> {
            self.ensure_owner()?;
            let Some(mut core) = self.core.get() else {
                return Err(Error::CoreNotConfigured);
            };
            core.settings = settings;
            self.core.set(&core);
            Ok(())
        }

        /// Configures the rollup target (only owner).
        ///
        /// @category Configuration
        ///
        #[ink(message)]
        pub fn config_client(&mut self, rpc: String, client_addr: Vec<u8>) -> Result<()> {
            self.ensure_owner()?;
            self.client = Some(Client {
                rpc,
                client_addr: client_addr
                    .try_into()
                    .or(Err(Error::InvalidAddressLength))?,
            });
            Ok(())
        }

        /// Get the final script to eval, for debugging.
        ///
        /// @category Configuration
        ///
        #[ink(message)]
        pub fn get_core_script(&self) -> Result<String> {
            let Some(Core { script, .. }) = self.core.get() else {
                return Err(Error::CoreNotConfigured);
            };
            Ok(build_final_js(
                script.clone(),
                logging::tagged_prefix().unwrap_or_default(),
            ))
        }

        ///
        /// @category Configuration
        ///
        #[ink(message)]
        pub fn is_ready(&self) -> Result<bool> {
            if self.client.is_some() && self.core.get().is_some() {
                Ok(true)
            } else {
                Ok(false)
            }
        }

        /// Get all configuration as once (only owner).
        ///
        /// It help reduce to total roundtrip when we building dApp on top of it.
        ///
        /// @category Configuration
        ///
        #[ink(message)]
        pub fn get_configuration(&self) -> Result<Configuration> {
            self.ensure_owner()?;
            let rpc = self.client.as_ref().map(|c| c.rpc.clone());
            let client_addr = self.client.as_ref().map(|c| c.client_addr);
            let config = if let Some(Core {
                script,
                settings,
                code_hash,
            }) = self.core.get()
            {
                Configuration {
                    rpc,
                    client_addr,
                    script: Some(script),
                    settings: Some(settings),
                    code_hash: Some(code_hash),
                }
            } else {
                Configuration {
                    rpc,
                    client_addr,
                    script: None,
                    settings: None,
                    code_hash: None,
                }
            };
            Ok(config)
        }

        /// Transfers the ownership of the contract (only owner).
        ///
        /// Transfer this to non-existent owner to renounce ownership and lock the configuration
        ///
        /// @category Metadata
        ///
        #[ink(message)]
        pub fn transfer_ownership(&mut self, new_owner: AccountId) -> Result<()> {
            self.ensure_owner()?;
            self.owner = new_owner;
            Ok(())
        }

        /// Pop an element from the rollup queue if any and process it, then submit the answer.
        ///
        /// @category Answer
        ///
        #[ink(message)]
        pub fn answer_request(&self) -> Result<Option<Vec<u8>>> {
            use pink_kv_session::traits::QueueSession;
            let client = self.ensure_client_configured()?;

            let mut rollup_client = connect(client)?;
            // Get a request if presents
            let request = rollup_client
                .session()
                .pop()
                .log_err("answer_request: failed to read queue")
                .or(Err(Error::FailedToGetStorage))?
                .ok_or(Error::NoRequestInQueue)?;
            let (reply, _hash) = self.handle_request(&request)?;
            // TODO: submit tx with code hash
            rollup_client.action(Action::Reply(reply));
            maybe_submit_tx(
                rollup_client,
                self.attest_key,
                self.brick_profile,
                client.rpc.clone(),
            )
        }

        /// Processes a request with the the core js and returns the output wrapped in a signed meta tx.
        ///
        /// @category Answer
        ///
        #[ink(message)]
        pub fn get_answer(&self, request: Vec<u8>) -> Result<Vec<u8>> {
            let client = self.ensure_client_configured()?;
            let (reply, _js_hash) = self.handle_request(&request)?;
            let (tx, sig) = sign_meta_tx(
                &client.rpc,
                client.client_addr.into(),
                &reply,
                &KeyPair::from(self.attest_key),
            )
            .log_err("failed to sign transaction")
            .or(Err(Error::FailedToSignTransaction))?;
            Ok(ethabi::encode(&[tx, Token::Bytes(sig.0)]))
        }

        /// Processes a request with the the core js and returns the output wrapped in a signed meta tx.
        ///
        /// The output is a tuple of the reply and the sha256 hash of the core js.
        /// The hash can be used to verify the integrity of the core js.
        ///
        /// @category Answer
        ///
        #[ink(message)]
        pub fn get_answer_with_code_hash(&self, request: Vec<u8>) -> Result<Vec<u8>> {
            let client = self.ensure_client_configured()?;
            let (reply, js_hash) = self.handle_request(&request)?;
            let data = ethabi::encode(&[Token::Bytes(reply), Token::FixedBytes(js_hash.to_vec())]);
            let (tx, sig) = sign_meta_tx(
                &client.rpc,
                client.client_addr.into(),
                &data,
                &KeyPair::from(self.attest_key),
            )
            .log_err("failed to sign transaction")
            .or(Err(Error::FailedToSignTransaction))?;
            Ok(ethabi::encode(&[tx, Token::Bytes(sig.0)]))
        }

        /// Processes a request with the the core js and returns the output without signature.
        ///
        /// @category Answer
        ///
        #[ink(message)]
        pub fn get_raw_answer(&self, request: Vec<u8>) -> Result<(Vec<u8>, CodeHash)> {
            self.handle_request(&request)
        }

        /// Processes a request with the the core js and returns the output.
        fn handle_request(&self, request: &[u8]) -> Result<(Vec<u8>, CodeHash)> {
            let Some(Core {
                script,
                settings,
                code_hash,
            }) = self.core.get()
            else {
                error!("CoreNotConfigured");
                return Err(Error::CoreNotConfigured);
            };
            let log_prefix = logging::tagged_prefix().unwrap_or_default();
            let final_js = build_final_js(script, log_prefix);
            let args = alloc::vec![alloc::format!("0x{}", hex_fmt::HexFmt(request)), settings];
            let output = match js::eval(&final_js, &args) {
                Ok(output) => output,
                Err(e) => {
                    error!("Failed to eval the core js: {}", e);
                    return Err(Error::JsError(e));
                }
            };
            let output = match output {
                js::Output::String(bytes) => hex::decode(bytes.as_str().trim_start_matches("0x"))
                    .map_err(|_| Error::InvalidJsOutput)?,
                js::Output::Bytes(b) => b,
            };
            Ok((output, code_hash))
        }

        /// Returns BadOrigin error if the caller is not the owner.
        fn ensure_owner(&self) -> Result<()> {
            if self.env().caller() == self.owner {
                Ok(())
            } else {
                Err(Error::BadOrigin)
            }
        }

        /// Returns the client config reference or raise the error `ClientNotConfigured`.
        fn ensure_client_configured(&self) -> Result<&Client> {
            self.client.as_ref().ok_or(Error::ClientNotConfigured)
        }

        fn config_core_inner(&mut self, core_js: String, settings: String) {
            let code_hash = self
                .env()
                .hash_bytes::<ink::env::hash::Sha2x256>(core_js.as_bytes());
            // TODO: To avoid wasting storage, we can
            // - make a generic contract to store k-v pairs.
            // - use the hash as the key to store the js.
            // - store only hash in the app contract.
            self.core.set(&Core {
                script: core_js,
                settings,
                code_hash,
            });
        }
    }

    fn connect(client: &Client) -> Result<EvmRollupClient> {
        let client_addr: H160 = client.client_addr.into();
        EvmRollupClient::new(&client.rpc, client_addr)
            .log_err("failed to create rollup client")
            .or(Err(Error::FailedToCreateClient))
    }

    fn maybe_submit_tx(
        rollup_client: EvmRollupClient,
        attest_key: [u8; 32],
        brick_profile: AccountId,
        rpc: String,
    ) -> Result<Option<Vec<u8>>> {
        let maybe_submittable = rollup_client
            .commit()
            .log_err("failed to commit")
            .or(Err(Error::FailedToCommitTx))?;
        if let Some(submittable) = maybe_submittable {
            // get BrickProfile info
            let from_address = build_call::<pink::PinkEnvironment>()
                .call(brick_profile)
                .transferred_value(0)
                .exec_input(ExecutionInput::new(Selector::new(ink::selector_bytes!(
                    "get_current_evm_account_address"
                ))))
                .returns::<brick_profile::Result<H160>>()
                .invoke()
                .log_err("failed to get evm address from profile")
                .or(Err(Error::BadBrickProfile))?;

            let attest_pair = KeyPair::from(attest_key);
            let tx_req = submittable
                .build_meta_tx(&attest_pair, from_address)
                .log_err("failed to build rollup meta-tx")
                .or(Err(Error::FailedToCreateTransaction))?;

            let signed_tx = build_call::<pink::PinkEnvironment>()
                .call(brick_profile)
                .transferred_value(0)
                .exec_input(
                    ExecutionInput::new(Selector::new(ink::selector_bytes!(
                        "sign_evm_transaction"
                    )))
                    .push_arg(tx_req),
                )
                .returns::<brick_profile::Result<Vec<u8>>>()
                .invoke()
                .log_err("failed to sign tx from profile")
                .or(Err(Error::FailedToSignTransaction))?;

            // Actually submit the tx (no guarantee for success)
            let eth = Eth::new(PinkHttp::new(rpc));
            let tx_id = resolve_ready(eth.send_raw_transaction(signed_tx.into()))
                .map_err(|_| Error::FailedToSendTransaction)?;

            return Ok(Some(tx_id.encode()));
        }
        Ok(None)
    }

    fn build_final_js(script: String, log_prefix: String) -> String {
        let final_js = alloc::format!(
            r#"
            (function(){{
                const logPrefix = "[{log_prefix}]:";
                const originLog = console.log;
                const originWarn = console.warn;
                const originError = console.error;
                console.log = function(...args) {{
                    originLog(logPrefix, ...args);
                }};
                console.warn = function(...args) {{
                    originWarn(logPrefix, ...args);
                }};
                console.error = function(...args) {{
                    originError(logPrefix, ...args);
                }};
                console.debug = function(...args) {{
                    originLog(logPrefix, ...args);
                }};
                console.info = function(...args) {{
                    originLog(logPrefix, ...args);
                }};
                console.assert = console.clear = console.count = console.countReset = console.dir = console.dirxml = console.group = console.groupCollapsed = console.groupEnd = console.profile = console.profileEnd = console.table = console.time = console.timeEnd = console.timeLog = console.timeStamp = console.trace = function() {{
                    throw new Error("Console API not all implemented, please use console.log instead.");
                }};
            }}());
            {script}
        "#
        );
        final_js
    }
}
