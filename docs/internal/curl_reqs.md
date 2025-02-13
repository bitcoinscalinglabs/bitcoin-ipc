```sh
curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "createsubnet",
    "params": {
	    "min_validator_stake": 100000000,
	    "min_validators": 4,
	    "bottomup_check_period": 5,
	    "active_validators_limit": 4,
	    "min_cross_msg_fee": 200,
	    "whitelist": [
		    "5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268",
		    "851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4",
		    "b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0",
				"b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529"
	    ]
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "joinsubnet",
    "params": {
			"subnet_id": "/b4/t420fepbcc2ait3aclq2exb3nmwmi4wmd5gfixnktv36mxmax5lmhpdr6qge5su",
			"collateral": 200000000,
			"ip": "66.222.44.55:8080",
			"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
			"pubkey": "5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "joinsubnet",
    "params": {
			"subnet_id": "/b4/t420fepbcc2ait3aclq2exb3nmwmi4wmd5gfixnktv36mxmax5lmhpdr6qge5su",
			"collateral": 110000000,
			"ip": "66.222.44.55:8081",
			"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
			"pubkey": "851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "joinsubnet",
    "params": {
			"subnet_id": "/b4/t420fepbcc2ait3aclq2exb3nmwmi4wmd5gfixnktv36mxmax5lmhpdr6qge5su",
			"collateral": 150000000,
			"ip": "66.222.44.55:8082",
			"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
			"pubkey": "b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "joinsubnet",
    "params": {
			"subnet_id": "/b4/t420fepbcc2ait3aclq2exb3nmwmi4wmd5gfixnktv36mxmax5lmhpdr6qge5su",
			"collateral": 180000000,
			"ip": "66.222.44.55:8083",
			"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
			"pubkey": "b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
	"jsonrpc": "2.0",
	"method": "getgenesisinfo",
	"params": {
		"subnet_id": "/b4/t420f7gmk32wp44h5kxcc2sbonm6iysmmkvfscmxr74kx4cqrofc4len4quzaha"
	},
	"id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "prefundsubnet",
    "params": {
			"subnet_id": "/b4/t420feejlnrllx3nr4tqfl5iuvwllwnazdiwehluntld6lc2z6w6ypffau6qppq",
			"amount": 40000000,
			"address": "0xbce2f194e9628e6ae06fa0d85dd57cd5579213bf"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "fundsubnet",
    "params": {
			"subnet_id": "/b4/t420f7gmk32wp44h5kxcc2sbonm6iysmmkvfscmxr74kx4cqrofc4len4quzaha",
			"amount": 40000000,
			"address": "0xbce2f194e9628e6ae06fa0d85dd57cd5579213bf"
    },
    "id": 1
}' | jq
```
