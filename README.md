# Openmina Archive

## Introduction

Openmina blockchain is a succinct blockchain that uses "scan state" to trade space for time. It allows smaller block time when the time to produce zk-snark is significantly higher, but requires the user to keep $n$ last blocks. On the Berkeley testnet $n = 290$.

### Genesis Ledger and Genesis Block

At the beginning we only have the Genesis ledger, which is the list of accounts. In Berkeley testnet it is hardcoded in `json` format. However, the `json` doesn't include an additional account of the Genesis block winner, in other words the account that wins the right to produce the Genesis block. This account is hardcoded separately.

This account must be at the position `0`, here is the value of the account on Berkeley.

```Rust
    list.insert(0, {
        let mut account = Account::empty();
        account.public_key = CompressedPubKey::from_address(
            "B62qiy32p8kAKnny8ZFwoMhYpBppM1DWVCqAPBYNcXnsAHhnfAAuXgg",
        )
        .unwrap();

        account.balance = Balance::of_nanomina_int_exn(1000);
        account.delegate = Some(account.public_key.clone());
        account
    });
```

The first block is called the Genesis block, it doesn't introduce any transactions and exists only to specify the initial consensus state. In the Genesis block the `slot` is equal to `0` and the `blockchain_length` is equal to  `1`. The Genesis block is not applied in the usual way, but it needed to apply the next block. To apply each block we need the previous block. The Genesis block is not applied because there is no previous block.

### Snarked Ledger and Staged Ledger

The ledger that has a zk-snark proof is a snarked ledger. The snarked ledger with a scan state where the zk-snark proof is not ready yet is a staged ledger.

Here I will use a mathematical-like notation that is suitable for this specification, but does not pretend to be complete.

There is an additional object called the $aux_n$ object, which is the difference between the snarked ledger and the staged ledger. It is essentially, the scan state at the point $n$ in the blockchain. We can think of $aux_n$ as a function that converts the snarked ledger to the staged ledger at $n$.

$ aux_n: snarked_n \to staged_n $

Also, the block is the function that updates the staged ledger (and snarked ledger) at $n$.

$ block_n: staged_{n - 1} \to staged_n $

where $n$ is `blockchain_length`. On each block, there is both a snarked ledger and a staged ledger. On the genesis block, they are the same and equal to the genesis ledger.

### Block formats

There are two block formats.

* P2P format, which is the data that peers exchange. It can be obtained using the RPC `get_transition_chain`. The Rust crate `mina-libp2p-messages` has a type `v2::MinaBlockBlockStableV2` for a block. It is usually binprot encoded, but can be json.
* And so-called graphql format, the data available in graphql, usually it is in json format.

## Archive

### From the Genesis block

For this we only need the Genesis ledger and all the blocks started from the Genesis block (including the Genesis block). Usually the Genesis ledger is hardcoded in the source code, so there is no need to store it.

There is a third-party service that provides all blocks in graphql json format. Unfortunately, this service is unreliable. Also, the current `rampup4` branch has a bug there a part is missing in the graphql block, so the block cannot be applied.

To have a complete archive starting from the genesis block, we need to capture all blocks in p2p format.

### From any block

To do this we need snarked ledger, `staged_ledger_aux_and_pending_coinbase` at some point $n$ and all blocks from $n$.

### Advanced testing

To test further, we should bootstrap the chain from any point. For this we need a snarked ledger and `staged_ledger_aux_and_pending_coinbase` at any `blockchain_length`.

To reduce database size, we may need to introduce snarked ledger diff, which is the list of changed accounts. This way, we can store all ledgers on all blocks and use less database space.

## Reliability

The archive tool must be running and connected to peers. However it is allowed to pause for a certain amount of time, let's call it `offline_time`. Peers are required to keep $n$ last blocks where the snarked ledger is not yet finalized.

```offline_time = block_time * n - bootstrap_time```

For Berkeley testnet `block_time` is 180 seconds, `n` is 290 and `bootstrap_time` is (in worst case) 1800 seconds. So `offline_time` is around 14 hours.

If the archive tool has crashed or lost connection for any reason, it must be fixed in 14 hours.
