#! /usr/bin/env bash
set -e;
set -o pipefail;

function rpc_call() {
     printf "Request:\n${1}\nReply:\n"
     curl -s -X POST \
          -H 'Content-Type: application/json' \
          -d "${1}" \
          http://127.0.0.1:9545
     printf "\n\n"
}

rpc_call '[{"jsonrpc":"2.0","id":"0","method":"starknet_getBlockByHash","params":["latest"]},
{"jsonrpc":"2.0","id":"1","method":"starknet_getBlockByHash","params":["0x7d328a71faf48c5c3857e99f20a77b18522480956d1cd5bff1ff2df3c8b427b"]}]'

rpc_call '[{"jsonrpc":"2.0","id":"2","method":"starknet_getBlockByNumber","params":["latest"]},
{"jsonrpc":"2.0","id":"3","method":"starknet_getBlockByNumber","params":[5000]}]'

# TODO not implemented yet
# rpc_call '[{"jsonrpc":"2.0","id":"4","method":"starknet_getStateUpdateByHash","params":["latest"]},
# {"jsonrpc":"2.0","id":"1","method":"starknet_getStateUpdateByHash","params":["0x7d328a71faf48c5c3857e99f20a77b18522480956d1cd5bff1ff2df3c8b427b"]}]'

rpc_call '[{"jsonrpc":"2.0","id":"5","method":"starknet_getStorageAt","params":["0x6fbd460228d843b7fbef670ff15607bf72e19fa94de21e29811ada167b4ca39", "0x0206F38F7E4F15E87567361213C28F235CCCDAA1D7FD34C9DB1DFE9489C6A091", "latest"]},
{"jsonrpc":"2.0","id":"6","method":"starknet_getStorageAt","params":["0x6fbd460228d843b7fbef670ff15607bf72e19fa94de21e29811ada167b4ca39", "0x0206F38F7E4F15E87567361213C28F235CCCDAA1D7FD34C9DB1DFE9489C6A091", "0x3871c8a0c3555687515a07f365f6f5b1d8c2ae953f7844575b8bde2b2efed27"]}]'

rpc_call '{"jsonrpc":"2.0","id":"7","method":"starknet_getTransactionByHash","params":["0x74ec6667e6057becd3faff77d9ab14aecf5dde46edb7c599ee771f70f9e80ba"]}'

rpc_call '[{"jsonrpc":"2.0","id":"8","method":"starknet_getTransactionByBlockHashAndIndex","params":["latest", 0]},
{"jsonrpc":"2.0","id":"9","method":"starknet_getTransactionByBlockHashAndIndex","params":["0x3871c8a0c3555687515a07f365f6f5b1d8c2ae953f7844575b8bde2b2efed27", 4]},
{"jsonrpc":"2.0","id":"10","method":"starknet_getTransactionByBlockNumberAndIndex","params":["latest", 0]},
{"jsonrpc":"2.0","id":"11","method":"starknet_getTransactionByBlockNumberAndIndex","params":[21348, 4]}]'

rpc_call '{"jsonrpc":"2.0","id":"12","method":"starknet_getTransactionReceipt","params":["0x74ec6667e6057becd3faff77d9ab14aecf5dde46edb7c599ee771f70f9e80ba"]}'

rpc_call '{"jsonrpc":"2.0","id":"13","method":"starknet_getCode","params":["0x6fbd460228d843b7fbef670ff15607bf72e19fa94de21e29811ada167b4ca39"]}'

rpc_call '[{"jsonrpc":"2.0","id":"14","method":"starknet_getBlockTransactionCountByHash","params":["latest"]},
{"jsonrpc":"2.0","id":"15","method":"starknet_getBlockTransactionCountByHash","params":["0x3871c8a0c3555687515a07f365f6f5b1d8c2ae953f7844575b8bde2b2efed27"]},
{"jsonrpc":"2.0","id":"16","method":"starknet_getBlockTransactionCountByNumber","params":["latest"]},
{"jsonrpc":"2.0","id":"17","method":"starknet_getBlockTransactionCountByNumber","params":[21348]}]'

rpc_call '{"jsonrpc":"2.0","id":"18","method":"starknet_call","params":[{"calldata":[1234],"contract_address":"0x6fbd460228d843b7fbef670ff15607bf72e19fa94de21e29811ada167b4ca39",
"entry_point_selector":"0x362398bec32bc0ebb411203221a35a0301193a96f317ebe5e40be9f60d15320","signature":[]}, "latest"]}'

rpc_call '{"jsonrpc":"2.0","id":"19","method":"starknet_blockNumber"}'

# TODO not implemented yet
# rpc_call '{"jsonrpc":"2.0","id":"20","method":"starknet_chainId"}'
# rpc_call '{"jsonrpc":"2.0","id":"21","method":"starknet_pendingTransactions"}'
# rpc_call '{"jsonrpc":"2.0","id":"22","method":"starknet_protocolVersion"}'
# rpc_call '{"jsonrpc":"2.0","id":"23","method":"starknet_syncing"}'
