if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <subnet_id>"
    exit 1
fi

SUBNET_ID=$1

# Resolve ipc-cli: use PATH first, else sibling ipc repo (relative to script location)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IPC_CLI=""
if command -v ipc-cli >/dev/null 2>&1; then
    IPC_CLI="ipc-cli"
elif [ -x "$SCRIPT_DIR/../ipc/target/release/ipc-cli" ]; then
    IPC_CLI="$SCRIPT_DIR/../ipc/target/release/ipc-cli"
else
    echo "Error: ipc-cli not found. Build it in the ipc repo (cargo build --release) or run from the container." >&2
    exit 1
fi

clear
while true; do
	clear # tput cup 0 0
    "$IPC_CLI" wallet balances --subnet "$SUBNET_ID" --wallet-type btc
    sleep 2
done
