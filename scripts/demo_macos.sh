#!/bin/bash

osascript -e 'tell app "Terminal"
    do script "bitcoind --printtoconsole --regtest --maxtxfee=50 --mintxfee=0.001"
end tell'

sleep 1

osascript -e 'tell app "Terminal"
    do script "cargo run --bin btc_monitor"
end tell'

sleep 1

osascript -e 'tell app "Terminal"
    do script "cargo run --bin l1_manager"
end tell'

sleep 1

for subnet_name in "$@"
do
    osascript -e 'tell app "Terminal"
        do script "cargo run --bin subnet_interactor -- --subnet-name '$subnet_name'"
    end tell'
done

echo "All processes started."
