import {
  createPublicClient,
  http,
  keccak256,
  numberToHex,
  TransactionNotFoundError,
  type Hex as ViemHex,
  type Transaction,
} from "viem";
import type { EvmChainConfig } from "./config.js";
import { TRANSFER_TOPIC } from "./deposits.js";
import type { BlockTx, EvmReader, Hex, RawLog } from "./types.js";

interface RpcLog {
  address: Hex;
  topics: Hex[];
  data: Hex;
  transactionHash: Hex | null;
  transactionIndex: string | number | null;
  logIndex: string | number | null;
  blockNumber: string | null;
}

/** JSON-RPC quantities are hex strings; viem's getLogs would have decoded them, raw calls do not. */
function rpcInt(v: string | number | null): number {
  if (v === null) return 0;
  return typeof v === "number" ? v : Number(BigInt(v));
}

/** Topic filter value for an address left-padded to 32 bytes. */
export function addressTopic(address: Hex): Hex {
  return ("0x" + "0".repeat(24) + address.slice(2).toLowerCase()) as Hex;
}

/** Returns an async gate that resolves at most `requestsPerSecond` times per second. */
export function createRateLimiter(requestsPerSecond: number, now = () => Date.now()): () => Promise<void> {
  const interval = 1000 / requestsPerSecond;
  let next = 0;
  return async () => {
    const current = now();
    const at = Math.max(current, next);
    next = at + interval;
    const wait = at - current;
    if (wait > 0) await new Promise((r) => setTimeout(r, wait));
  };
}

export function createEvmReader(chain: EvmChainConfig): EvmReader {
  const client = createPublicClient({
    transport: http(chain.rpcUrl, { retryCount: 4, retryDelay: 1_000, timeout: 30_000 }),
  });
  const gate = createRateLimiter(chain.requestsPerSecond);
  const vault = chain.vault;

  return {
    async blockNumber() {
      await gate();
      return client.getBlockNumber();
    },

    async blockTransactions(blockNumber: bigint): Promise<BlockTx[]> {
      await gate();
      const block = await client.getBlock({ blockNumber, includeTransactions: true });
      const txs = block.transactions as readonly Transaction[];
      return txs.map((tx) => ({
        hash: tx.hash as Hex,
        from: tx.from as Hex,
        to: (tx.to ?? null) as Hex | null,
        value: tx.value,
        transactionIndex: Number(tx.transactionIndex ?? 0),
        blockNumber,
      }));
    },

    async receiptStatus(txHash: Hex) {
      await gate();
      try {
        const receipt = await client.getTransactionReceipt({ hash: txHash as ViemHex });
        return receipt.status;
      } catch (err) {
        if (err instanceof Error && err.name === "TransactionReceiptNotFoundError") return null;
        throw err;
      }
    },

    async transferLogs(fromBlock: bigint, toBlock: bigint): Promise<RawLog[]> {
      await gate();
      // Raw eth_getLogs: no address filter, so every token's Transfer-to-vault is returned and
      // the watcher decides what is registered. viem's getLogs cannot express a bare topic filter.
      const logs = (await client.request({
        method: "eth_getLogs",
        params: [
          {
            fromBlock: numberToHex(fromBlock),
            toBlock: numberToHex(toBlock),
            topics: [TRANSFER_TOPIC, null, addressTopic(vault)],
          },
        ],
      } as never)) as RpcLog[];
      const out: RawLog[] = [];
      for (const l of logs) {
        if (!l.transactionHash || l.logIndex === null || l.blockNumber === null) continue;
        out.push({
          address: l.address,
          topics: l.topics,
          data: l.data,
          transactionHash: l.transactionHash,
          transactionIndex: rpcInt(l.transactionIndex),
          logIndex: rpcInt(l.logIndex),
          blockNumber: BigInt(l.blockNumber),
        });
      }
      return out;
    },

    async nonce(address: Hex, blockTag: "latest" | "pending") {
      await gate();
      return client.getTransactionCount({ address: address as ViemHex, blockTag });
    },

    async feeData() {
      await gate();
      const block = await client.getBlock({ blockTag: "latest" });
      await gate();
      const priorityFeePerGas = await client.estimateMaxPriorityFeePerGas();
      return { baseFeePerGas: block.baseFeePerGas ?? 0n, priorityFeePerGas };
    },

    async hasTransaction(txHash: Hex) {
      await gate();
      try {
        await client.getTransaction({ hash: txHash as ViemHex });
        return true;
      } catch (err) {
        if (err instanceof TransactionNotFoundError) return false;
        throw err;
      }
    },

    async sendRawTransaction(raw: Hex) {
      await gate();
      return (await client.sendRawTransaction({ serializedTransaction: raw as ViemHex })) as Hex;
    },
  };
}

/** keccak256 of a signed raw transaction: its hash on chain. */
export function txHashOf(raw: Hex): Hex {
  return keccak256(raw as ViemHex) as Hex;
}
