#!/bin/bash

gnome-terminal --title="bitcoind" -- bash -c "bitcoind --printtoconsole --regtest --maxtxfee=50 --mintxfee=0.001; exec bash"

sleep 1

gnome-terminal --title="btc_monitor" -- bash -c "cargo run --bin btc_monitor; exec bash"

sleep 1

gnome-terminal --title="l1_manager" -- bash -c "cargo run --bin l1_manager; exec bash"

sleep 1

gnome-terminal --title="generate_keypair" -- bash -c "cargo run --bin generate_keypair; exec bash"

for subnet_name in "$@"
do
    gnome-terminal --title="subnet_interactor $subnet_name" -- bash -c "cargo run --bin subnet_interactor -- --subnet-name $subnet_name; exec bash"
done

echo "All processes started."
