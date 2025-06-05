# create wallets
bitcoin-cli createwallet "default"
bitcoin-cli createwallet "validator1"
bitcoin-cli createwallet "validator2"
bitcoin-cli createwallet "validator3"
bitcoin-cli createwallet "validator4"
bitcoin-cli createwallet "validator5"
bitcoin-cli createwallet "validator6"
bitcoin-cli createwallet "user1"
bitcoin-cli createwallet "user2"

# fund wallets
bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=validator1 getnewaddress)"
bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=validator2 getnewaddress)"
bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=validator3 getnewaddress)"
bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=validator4 getnewaddress)"
bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=validator5 getnewaddress)"
bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=validator6 getnewaddress)"
bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=user1 getnewaddress)"
bitcoin-cli generatetoaddress 2 "$(bitcoin-cli --rpcwallet=user2 getnewaddress)"
bitcoin-cli generatetoaddress 102 "$(bitcoin-cli --rpcwallet=default getnewaddress)"

# check balances
bitcoin-cli --rpcwallet=default getbalance
bitcoin-cli --rpcwallet=validator1 getbalance
bitcoin-cli --rpcwallet=validator2 getbalance
bitcoin-cli --rpcwallet=validator3 getbalance
bitcoin-cli --rpcwallet=validator4 getbalance
bitcoin-cli --rpcwallet=validator5 getbalance
bitcoin-cli --rpcwallet=validator6 getbalance
bitcoin-cli --rpcwallet=user1 getbalance
bitcoin-cli --rpcwallet=user2 getbalance