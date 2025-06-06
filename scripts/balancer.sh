if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <subnet_id>"
    exit 1
fi

SUBNET_ID=$1

clear
while true; do
	clear # tput cup 0 0
    ../ipc/target/release/ipc-cli wallet balances --subnet "$SUBNET_ID" --wallet-type btc
    sleep 2
done
