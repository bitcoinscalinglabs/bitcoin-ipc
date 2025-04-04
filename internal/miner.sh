 while true; do
     bitcoin-cli generatetoaddress 1 "$(bitcoin-cli -rpcwallet=default getnewaddress)" > /dev/null
     echo "Mined a new block $(bitcoin-cli getblockcount)"
     sleep 20
 done
