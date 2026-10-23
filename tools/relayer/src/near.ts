import { readFileSync } from "node:fs";
import { Account } from "@near-js/accounts";
import { JsonRpcProvider } from "@near-js/providers";
import { KeyPairSigner } from "@near-js/signers";
import type { NearConfig } from "./config.js";
import type {
  AssetInfo,
  EvmChainInfo,
  ExecuteArgs,
  ExecuteOutcome,
  IntentCancelArgs,
  IntentRecord,
  IntentSubmitArgs,
  NearReader,
  NearWriter,
  ReportDepositArgs,
  WithdrawalRecord,
} from "./types.js";

const TGAS = 1_000_000_000_000n;

/** Read-only NEAR JSON-RPC view calls (same pattern as sdk/src/client/rpc.ts, copied to stay standalone). */
export function createNearReader(cfg: NearConfig, fetchImpl: typeof fetch = fetch): NearReader {
  async function view<T>(method: string, args: Record<string, unknown>): Promise<T> {
    const body = {
      jsonrpc: "2.0",
      id: "windswap-relayer",
      method: "query",
      params: {
        request_type: "call_function",
        finality: "final",
        account_id: cfg.contractId,
        method_name: method,
        args_base64: Buffer.from(JSON.stringify(args), "utf8").toString("base64"),
      },
    };
    const res = await fetchImpl(cfg.rpcUrl, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) throw new Error(`NEAR RPC HTTP ${res.status} for ${method}`);
    const json = (await res.json()) as {
      error?: { message?: string };
      result?: { result?: number[]; error?: string };
    };
    if (json.error) throw new Error(`NEAR RPC error for ${method}: ${json.error.message ?? "unknown"}`);
    const result = json.result;
    if (!result) throw new Error(`NEAR RPC returned no result for ${method}`);
    if (result.error) throw new Error(`${method} failed: ${result.error}`);
    if (!result.result) throw new Error(`NEAR RPC returned no bytes for ${method}`);
    return JSON.parse(new TextDecoder().decode(Uint8Array.from(result.result))) as T;
  }

  return {
    getEvmAssets: () => view<AssetInfo[]>("get_evm_assets", {}),
    getEvmChain: (chainId) => view<EvmChainInfo | null>("get_evm_chain", { chain_id: chainId }),
    getPendingWithdrawals: (fromId, limit) =>
      view<WithdrawalRecord[]>("get_pending_withdrawals", { from_id: fromId, limit }),
    getSignedWithdrawals: (fromId, limit) =>
      view<WithdrawalRecord[]>("get_signed_withdrawals", { from_id: fromId, limit }),
    getWithdrawal: (id) => view<WithdrawalRecord | null>("get_withdrawal", { id }),
    resolveWallet: (wallet) => view<string>("resolve_wallet", { wallet }),
    getIntent: (id) => view<IntentRecord | null>("get_intent", { id }),
    getIntentsOfOwner: (owner, fromIndex, limit) =>
      view<IntentRecord[]>("get_intents_of_owner", { owner, from_index: fromIndex, limit }),
  };
}

interface CredentialsFile {
  account_id?: string;
  public_key?: string;
  private_key: string;
}

/** Change calls signed by the relayer's NEAR account. Never constructed in dry-run. */
export function createNearWriter(cfg: NearConfig): NearWriter {
  const creds = JSON.parse(readFileSync(cfg.credentialsPath, "utf8")) as Partial<CredentialsFile>;
  if (!creds.private_key) {
    throw new Error(`credentials file ${cfg.credentialsPath} must contain private_key`);
  }
  // Seed-phrase keychain files (master_seed_phrase, implicit_account_id) have no account_id;
  // the configured relayerAccountId is authoritative in that case.
  if (creds.account_id !== undefined && creds.account_id !== cfg.relayerAccountId) {
    throw new Error(`credentials are for ${creds.account_id}, config relayerAccountId is ${cfg.relayerAccountId}`);
  }
  const provider = new JsonRpcProvider({ url: cfg.rpcUrl });
  const signer = KeyPairSigner.fromSecretKey(creds.private_key as `ed25519:${string}`);
  const account = new Account(cfg.relayerAccountId, provider, signer);

  async function change(
    methodName: string,
    args: Record<string, unknown>,
    tgas: number,
    attachedDeposit = 0n,
  ): Promise<unknown> {
    const outcome = await account.functionCall({
      contractId: cfg.contractId,
      methodName,
      args,
      gas: BigInt(tgas) * TGAS,
      attachedDeposit,
    });
    const success = (outcome.status as { SuccessValue?: string }).SuccessValue;
    if (success === undefined || success === "") return undefined;
    return JSON.parse(Buffer.from(success, "base64").toString("utf8")) as unknown;
  }

  /** A signed-relay call: throws on a failed outcome, returns the tx hash and the decoded return value. */
  async function signedCall(methodName: string, args: Record<string, unknown>, tgas: number): Promise<ExecuteOutcome> {
    const outcome = await account.functionCall({
      contractId: cfg.contractId,
      methodName,
      args,
      gas: BigInt(tgas) * TGAS,
      attachedDeposit: 0n,
    });
    const failure = failureText(outcome.status);
    if (failure !== null) throw new Error(failure);
    const txHash = (outcome as { transaction?: { hash?: string } }).transaction?.hash;
    if (!txHash) throw new Error("NEAR outcome has no transaction hash");
    const success = (outcome.status as { SuccessValue?: string }).SuccessValue;
    const result =
      success === undefined || success === ""
        ? undefined
        : (JSON.parse(Buffer.from(success, "base64").toString("utf8")) as unknown);
    return result === undefined ? { txHash } : { txHash, result };
  }

  return {
    executeAction: (args: ExecuteArgs) => signedCall("execute", { ...args }, cfg.gasTgas.execute),
    submitIntent: (args: IntentSubmitArgs) => signedCall("submit_intent", { ...args }, cfg.gasTgas.submitIntent),
    cancelIntent: (args: IntentCancelArgs) => signedCall("cancel_intent", { ...args }, cfg.gasTgas.cancelIntent),
    async reportEvmDeposit(args: ReportDepositArgs) {
      const value = await change("report_evm_deposit", { ...args }, cfg.gasTgas.report);
      return value === true;
    },
    async processWithdrawal(args) {
      await change("process_withdrawal", { ...args }, cfg.gasTgas.process, 1n);
    },
    async markWithdrawalBroadcast(args) {
      await change("mark_withdrawal_broadcast", { ...args }, cfg.gasTgas.mark);
    },
  };
}

/**
 * Text describing a failed execution, or null when the status is not a failure. Contract panics
 * are rendered as `Smart contract panicked: <message>` so `isContractRejection` classifies them.
 */
function failureText(status: unknown): string | null {
  const failure = (status as { Failure?: unknown }).Failure;
  if (failure === undefined) return null;
  const text = JSON.stringify(failure);
  const panic = /panicked: ([^"\\]*)/.exec(text);
  if (panic?.[1]) return `Smart contract panicked: ${panic[1].trim()}`;
  return `NEAR transaction failed: ${text.slice(0, 500)}`;
}

/** True when the NEAR error is a contract-level rejection (panic) rather than an RPC/network failure. */
export function isContractRejection(err: unknown): boolean {
  if (!(err instanceof Error)) return false;
  const type = (err as { type?: unknown }).type;
  const text = `${err.name} ${err.message} ${typeof type === "string" ? type : ""}`;
  return /panicked|FunctionCallError|ExecutionError|Smart contract/i.test(text);
}
