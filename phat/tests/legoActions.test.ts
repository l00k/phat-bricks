import * as PhalaSdk from "@phala/sdk";
import { ApiPromise } from "@polkadot/api";
import type { KeyringPair } from "@polkadot/keyring/types";
import {
  ContractFactory,
  RuntimeContext,
  TxHandler,
} from "devphase";
import { PinkSystem } from "@/typings/PinkSystem";
import { Lego } from "@/typings/Lego";
import { ActionEvmTransaction } from "@/typings/ActionEvmTransaction"

import "dotenv/config";

async function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function checkUntil(async_fn, timeout) {
  const t0 = new Date().getTime();
  while (true) {
    if (await async_fn()) {
      return;
    }
    const t = new Date().getTime();
    if (t - t0 >= timeout) {
      throw new Error("timeout");
    }
    await sleep(100);
  }
}

describe("Run lego actions", () => {
  let systemFactory: PinkSystem.Factory;
  let system: PinkSystem.Contract;
  let qjsFactory: ContractFactory;
  let legoFactory: Lego.Factory;
  let lego: Lego.Contract;
  let evmTransactionFactory: ActionEvmTransaction.Factory;
  let evmTransaction: ActionEvmTransaction.Contract;

  let api: ApiPromise;
  let alice: KeyringPair;
  let certAlice: PhalaSdk.CertificateData;
  const txConf = { gasLimit: "10000000000000", storageDepositLimit: null };
  let currentStack: string;
  let systemContract: string;

  before(async function () {
    currentStack = (await RuntimeContext.getSingleton()).paths.currentStack;
    console.log("clusterId:", this.devPhase.mainClusterId);
    console.log(`currentStack: ${currentStack}`);

    api = this.api;
    const clusterInfo =
      await api.query.phalaFatContracts.clusters(
        this.devPhase.mainClusterId
      );
    systemContract = clusterInfo.unwrap().systemContract.toString();
    console.log("system contract:", systemContract);

    systemFactory = await this.devPhase.getFactory(`${currentStack}/system.contract`);
    qjsFactory = await this.devPhase.getFactory('qjs', {
      clusterId: this.devPhase.mainClusterId,
      contractType: "IndeterministicInkCode" as any,
    });
    legoFactory = await this.devPhase.getFactory('lego');
    evmTransactionFactory = await this.devPhase.getFactory('action_evm_transaction');

    await qjsFactory.deploy();
    await legoFactory.deploy();
    await evmTransactionFactory.deploy();

    alice = this.devPhase.accounts.alice;
    certAlice = await PhalaSdk.signCertificate({
      api,
      pair: alice,
    });
    console.log("Signer:", alice.address.toString());

    // register the qjs to JsDelegate driver
    system = (await systemFactory.attach(systemContract)) as any;
    await TxHandler.handle(
      system.tx["system::setDriver"](
        { gasLimit: "10000000000000" },
        "JsDelegate",
        qjsFactory.metadata.source.hash
      ),
      alice,
      true,
    );
    await checkUntil(async () => {
      const { output } = await system.query["system::getDriver"](
        certAlice,
        {},
        "JsDelegate"
      );
      return !output.isEmpty;
    }, 1000 * 10);
  });

  describe("Run actions", () => {
    before(async function () {
      this.timeout(30_000);
      // Deploy contract
      lego = await legoFactory.instantiate("default", [], {
        transferToCluster: 1e12,
      });
      evmTransaction = await evmTransactionFactory.instantiate("default", [], {});
      await sleep(3_000);
    });

    it("can run actions", async function () {
      const callee = evmTransaction.address.toHex();
      // pub fn config(&mut self, rpc: String) -> Result<()>
      const selectorConfig = 0x70714744;
      // pub fn build_transaction(
      //   &self,
      //   to: String,
      //   abi: Vec<u8>,
      //   func: String,
      //   params: Vec<Vec<u8>>,
      // ) -> Result<Vec<u8>>
      const selectorBuildTx = 0x8a688c06;
      function cfg(o: object) {
        return JSON.stringify(o);
      }

      // const actions_config_contract = `[
      //       { "cmd": "eval", "config": "scale.encode('test.rpc', scale.encodeStr)" },
      //       { "cmd": "call", "config": ${cfg({ callee, selectorConfig })}},
      // ]`;
      await TxHandler.handle(
        evmTransaction.tx.config({ gasLimit: "10000000000000" }, "test.rpc"),
        alice,
        true,
      );
      await checkUntil(async () => {
        const result = await evmTransaction.query.getRpc(certAlice, {});
        return result.result.isOk;
      }, 1000 * 10);

      const actions_json = `[
            {"cmd": "fetch", "config": ${cfg({
        returnTextBody: true,
        url: "https://min-api.cryptocompare.com/data/price?fsym=ETH&tsyms=BTC,USD,EUR",
      })}},
            {"cmd": "eval", "config": "BigInt(Math.round(JSON.parse(input.body).USD * 1000000))"},
            {"cmd": "eval", "config": "scale.encode(input, scale.encodeU128)"},
            {"cmd": "log"}
      ]`;
      const result = await lego.query.run(certAlice, {}, actions_json);
      expect(result.result.isOk).to.be.true;
      expect(!result.output?.valueOf().isEmpty).to.be.true;
    });
  });
});
