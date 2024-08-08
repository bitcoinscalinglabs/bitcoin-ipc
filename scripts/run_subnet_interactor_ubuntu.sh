#!/bin/bash

if [ -z "$1" ]; then
  echo "Error: No subnet_name provided."
  exit 1
fi

subnet_name=$1

gnome-terminal --title="subnet_interactor $subnet_name" -- bash -c "cargo run --bin subnet_interactor -- --subnet-name $subnet_name; exec bash"

echo "Subnet interactor for $subnet_name started."
