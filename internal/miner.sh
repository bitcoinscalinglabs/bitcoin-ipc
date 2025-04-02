 while true; do
     echo "Mining a new block..."
     bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)" > /dev/null
     sleep 40
 done
