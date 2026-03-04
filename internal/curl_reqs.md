```sh
curl -X POST http://localhost:3040/api \
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
			"subnet_id": "/b4/t410f7kn2c5qglq6ymzbqczbff2scqm2y6vszeqc2lxy",
			"collateral": 200000000,
			"ip": "66.222.44.55:8080",
			"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
			"pubkey": "5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268"
    },
    "id": 1
}' | jq

# join new validator

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "joinsubnet",
    "params": {
			"subnet_id": "/b4/t410f7kn2c5qglq6ymzbqczbff2scqm2y6vszeqc2lxy",
			"collateral": 20000000,
			"ip": "66.222.44.55:8080",
			"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
			"pubkey": "e327a66b169732bde49d827d90781327af558fc12d5cd2d5004e7551ec00c662"
    },
    "id": 1
}' | jq

# join new validator 2

-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "joinsubnet",
    "params": {
			"subnet_id": "/b4/t410fertekptrvemo3wddyaht6v2pqykjdjieorit6ha",
			"collateral": 21000000,
			"ip": "66.222.44.55:8080",
			"backup_address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n",
			"pubkey": "71ac1eb874233999e11cd050f388f1dd6da9b446180fdf9b06740419cc487b6f"
    },
    "id": 1
}' | jq

# stake more collateral

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "stakecollateral",
    "params": {
			"subnet_id": "/b4/t410f7kn2c5qglq6ymzbqczbff2scqm2y6vszeqc2lxy",
			"amount": 6500000,
			"pubkey": "851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "stakecollateral",
    "params": {
			"subnet_id": "/b4/t410f7kn2c5qglq6ymzbqczbff2scqm2y6vszeqc2lxy",
			"amount": 8500000,
			"pubkey": "b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0"
    },
    "id": 1
}' | jq

# stake more collateral 2

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "stakecollateral",
    "params": {
			"subnet_id": "/b4/t410f7kn2c5qglq6ymzbqczbff2scqm2y6vszeqc2lxy",
			"amount": 4500000,
			"pubkey": "e327a66b169732bde49d827d90781327af558fc12d5cd2d5004e7551ec00c662"
    },
    "id": 1
}' | jq

# unstake more collateral

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "unstakecollateral",
    "params": {
			"subnet_id": "/b4/t410f7kn2c5qglq6ymzbqczbff2scqm2y6vszeqc2lxy",
			"amount": 2000000
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
			"subnet_id": "/b4/t410f7kn2c5qglq6ymzbqczbff2scqm2y6vszeqc2lxy",
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
			"subnet_id": "/b4/t410f7kn2c5qglq6ymzbqczbff2scqm2y6vszeqc2lxy",
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
			"subnet_id": "/b4/t410f7kn2c5qglq6ymzbqczbff2scqm2y6vszeqc2lxy",
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
		"subnet_id": "/b4/t410fzmra6da4kf2xslwqgqjxgrh67zvd6mhujbqdosi"
	},
	"id": 1
}' | jq

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "getsubnet",
    "params": {
			"subnet_id": "/b4/t410fn7eqf4xnhatdjzwo4xhmidxrm4ty2xwbhhciuwy"
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
			"subnet_id": "/b4/t410fuvtcymuzlxqj4ypvbf7ybk4rdlkq6u4mqtcrybi",
			"amount": 40000000,
			"address": "0xbce2f194e9628e6ae06fa0d85dd57cd5579213bf"
    },
    "id": 1
}' | jq

# unstake more collateral

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "killsubnet",
    "params": {
			"subnet_id": /b4/t410flanywnfx3ynyhmt5fvdwqqlfzslgyr7bwyc5zba"
    },
    "id": 1
}' | jq

# get kill requests

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "getkillrequests",
    "params": {
      "subnet_id": "/b4/t410flanywnfx3ynyhmt5fvdwqqlfzslgyr7bwyc5zba"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "dev_killsubnet",
    "params": {
    		"subnet_id": "/b4/t410fn7eqf4xnhatdjzwo4xhmidxrm4ty2xwbhhciuwy",
        "secret_keys": [
            "21b16a87dd69bc6283045ab63738c9ab73c93c93f91e96cd0e54bd321bba80ad",
            "67308c2f3915f4c36135f267ed709418c2880025d669e4ada7a206842d53c146",
            "994220215e4601d21a245f8f5e0c407f2f5733ce7907e128c3190c64f4ef443c",
            "ab3a1fafa925836386be55b12fdc92f208ebdad5ef96c0109e4bd06638dcb897"
        ]
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "getrootnetmessages",
    "params": {
			"subnet_id": "/b4/t410f7kn2c5qglq6ymzbqczbff2scqm2y6vszeqc2lxy",
			"block_height": 235
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "getstakechanges",
    "params": {
			"subnet_id": "/b4/t410f7kn2c5qglq6ymzbqczbff2scqm2y6vszeqc2lxy",
			"block_height": 162
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "genmultisigspendpsbt",
    "params": {
			"subnet_id": "/b4/t420fxm3vljgrnt4az4nbhwo74ih3b4lce2ecfzfrytqtzfnulhjfuagct52yci",
			"recipient": "bcrt1q3pw5xfrph88qgd4uwmwgw5xh60np6mdcdd2h5k",
			"amount": 20000000
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3040/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "genbootstraphandover",
    "params": {
			"subnet_id": "/b4/t410f7eqjpzo3akekevrpbfdwwhkbj65pzaanskoml6i"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer validator1_auth_token" \
-d '{
    "jsonrpc": "2.0",
    "method": "genbootstraphandover",
    "params": {
			"subnet_id": "/b4/t410f7eqjpzo3akekevrpbfdwwhkbj65pzaanskoml6i"
    },
    "id": 1
}' | jq

# checkpoint with withdrawals and transfers

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "gencheckpointpsbt",
    "params": {
        "subnet_id": "/b4/t420fc4dyqkfru6jk5ybusvp7ybs4mn5arkajawoiv5bkb4wfssegugslaeixmm",
        "checkpoint_hash": "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        "withdrawals": [
            {
                "amount": 25000,
                "address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n"
            }
        ],
        "transfers": [
            {
                "amount": 150000,
                "destination_subnet_id": "/b4/t420f4pyvwv4erfqcqrjykznyu4zkyepp7v6ki2p2v2wug6bubrxrlpiwpxozzm",
                "subnet_user_address": "0xbce2f194e9628e6ae06fa0d85dd57cd5579213bf"
            },
            {
                "amount": 100000,
                "destination_subnet_id": "/b4/t420f4pyvwv4erfqcqrjykznyu4zkyepp7v6ki2p2v2wug6bubrxrlpiwpxozzm",
                "subnet_user_address": "0x4967bB72907683bb6a933d47348a49bC3832968b"
            }
        ]
    },
    "id": 1
}' | jq

# no transfers checkpoint

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "gencheckpointpsbt",
    "params": {
        "subnet_id": "/b4/t420fc4dyqkfru6jk5ybusvp7ybs4mn5arkajawoiv5bkb4wfssegugslaeixmm",
        "checkpoint_hash": "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f",
        "withdrawals": [
            {
                "amount": 25000,
                "address": "bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n"
            }
        ],
        "transfers": []
    },
    "id": 1
}' | jq

# no transfers no withdrawals

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "gencheckpointpsbt",
    "params": {
        "subnet_id": "/b4/t420fc4dyqkfru6jk5ybusvp7ybs4mn5arkajawoiv5bkb4wfssegugslaeixmm",
        "checkpoint_hash": "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
    },
    "id": 1
}' | jq

curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "dev_multisignpsbt",
    "params": {
        "unsigned_psbt_base64": "cHNidP8BAP1SAQIAAAABiZUUNBUjat9BZH6yhbFBVE01N39wx18nYv7bGCk9wNgBAAAAAP////8GAAAAAAAAAABLaklJUEM6Q1BUFweIKLGnkq7gNJVf/AZcY3oIqAkFnIr0Kg8sWUiGoaQAAAAAABnWaJwIWuFlgx6TT/djrkaipsFys/G2CozibwECqGEAAAAAAAAWABSKRTgEccvV0a9lany7HV7qjXmRW5gSAAAAAAAAIlEg65foRoWTTULdXwrR0E7GOxHyHdPpMXk3Jgme08GCfnLwSQIAAAAAACJRIKdn9Pj8p+KB3Ewsx/xFsoZK+IlU6OXxfS1+4VlmHXM7oIYBAAAAAAAiUSCnZ/T4/KfigdxMLMf8RbKGSviJVOjl8X0tfuFZZh1zO1r0RQwAAAAAIlEgmLN5C+hisXFbmnjZlOtkeIO+CAGxC7Zsvh8EYgTxUTEAAAAAAAEBK8BcSgwAAAAAIlEgmLN5C+hisXFbmnjZlOtkeIO+CAGxC7Zsvh8EYgTxUTEBBYogXw3+06UnrHQMfUpZTNOqEFmpNhhzmfxJ4/xupq4XcmisIIUcG9oydYRHnpinwo6nrcCX0pDv0QUxC89xQjG7mfr0uiCxX5mSjyR4oQxXOaA/VJXTQudzUtYk58yOv73tVE+awLogtF/VJXPo5r/gr/gvsijoh/3ZIhD+CVKuZaWQgP7H5Sm6U6JCFcB5vmZ++dy7rFWgYpXOhwsHApv82y3OKNlZ8oFbFvgXmR/A0V8sImCxylvl4wqDc9KyfpJ29BMIsGNeb/ogOBLwiyBfDf7TpSesdAx9SllM06oQWak2GHOZ/Enj/G6mrhdyaKwghRwb2jJ1hEeemKfCjqetwJfSkO/RBTELz3FCMbuZ+vS6ILFfmZKPJHihDFc5oD9UldNC53NS1iTnzI6/ve1UT5rAuiC0X9Ulc+jmv+Cv+C+yKOiH/dkiEP4JUq5lpZCA/sflKbpTosABFyB5vmZ++dy7rFWgYpXOhwsHApv82y3OKNlZ8oFbFvgXmQEYIJ04u+iZnmVuhe+3wJVCa+C8bG10fuKiKDaN1vZukXIoAAAAAAAAAA==",
        "secret_keys": [
            "21b16a87dd69bc6283045ab63738c9ab73c93c93f91e96cd0e54bd321bba80ad",
            "67308c2f3915f4c36135f267ed709418c2880025d669e4ada7a206842d53c146",
            "994220215e4601d21a245f8f5e0c407f2f5733ce7907e128c3190c64f4ef443c",
            "ab3a1fafa925836386be55b12fdc92f208ebdad5ef96c0109e4bd06638dcb897"
        ]
    },
    "id": 1
}' | jq


curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "finalizecheckpointpsbt",
    "params": {
        "subnet_id": "/b4/t420fc4dyqkfru6jk5ybusvp7ybs4mn5arkajawoiv5bkb4wfssegugslaeixmm",
        "unsigned_psbt_base64": "cHNidP8BAP1SAQIAAAABiZUUNBUjat9BZH6yhbFBVE01N39wx18nYv7bGCk9wNgBAAAAAP////8GAAAAAAAAAABLaklJUEM6Q1BUFweIKLGnkq7gNJVf/AZcY3oIqAkFnIr0Kg8sWUiGoaQAAAAAABnWaJwIWuFlgx6TT/djrkaipsFys/G2CozibwECqGEAAAAAAAAWABSKRTgEccvV0a9lany7HV7qjXmRW5gSAAAAAAAAIlEg65foRoWTTULdXwrR0E7GOxHyHdPpMXk3Jgme08GCfnLwSQIAAAAAACJRIKdn9Pj8p+KB3Ewsx/xFsoZK+IlU6OXxfS1+4VlmHXM7oIYBAAAAAAAiUSCnZ/T4/KfigdxMLMf8RbKGSviJVOjl8X0tfuFZZh1zO1r0RQwAAAAAIlEgmLN5C+hisXFbmnjZlOtkeIO+CAGxC7Zsvh8EYgTxUTEAAAAAAAEBK8BcSgwAAAAAIlEgmLN5C+hisXFbmnjZlOtkeIO+CAGxC7Zsvh8EYgTxUTEBBYogXw3+06UnrHQMfUpZTNOqEFmpNhhzmfxJ4/xupq4XcmisIIUcG9oydYRHnpinwo6nrcCX0pDv0QUxC89xQjG7mfr0uiCxX5mSjyR4oQxXOaA/VJXTQudzUtYk58yOv73tVE+awLogtF/VJXPo5r/gr/gvsijoh/3ZIhD+CVKuZaWQgP7H5Sm6U6JCFcB5vmZ++dy7rFWgYpXOhwsHApv82y3OKNlZ8oFbFvgXmR/A0V8sImCxylvl4wqDc9KyfpJ29BMIsGNeb/ogOBLwiyBfDf7TpSesdAx9SllM06oQWak2GHOZ/Enj/G6mrhdyaKwghRwb2jJ1hEeemKfCjqetwJfSkO/RBTELz3FCMbuZ+vS6ILFfmZKPJHihDFc5oD9UldNC53NS1iTnzI6/ve1UT5rAuiC0X9Ulc+jmv+Cv+C+yKOiH/dkiEP4JUq5lpZCA/sflKbpTosABFyB5vmZ++dy7rFWgYpXOhwsHApv82y3OKNlZ8oFbFvgXmQEYIJ04u+iZnmVuhe+3wJVCa+C8bG10fuKiKDaN1vZukXIoAAAAAAAAAA==",
        "signatures": [
        [
          "5f0dfed3a527ac740c7d4a594cd3aa1059a936187399fc49e3fc6ea6ae177268",
          [
            "f245679ccda14b190213d4115ba8c10d484d5f0d1e0a37a493bd88f9fce3f05b5514debb23e83c693a1fdeb0622970fc3691dbbdee87b7430af41acdca58f44c"
          ]
        ],
        [
          "851c1bda327584479e98a7c28ea7adc097d290efd105310bcf714231bb99faf4",
          [
            "14da8c21b0a6ab6218b27e8bec5f750de26c0f0fe1e0265b6788dcc0f1c059473547e1737d16e2238587e927a428fa1c5d20e460bedf4ae4af25bb245448c1b2"
          ]
        ],
        [
          "b15f99928f2478a10c5739a03f5495d342e77352d624e7cc8ebfbded544f9ac0",
          [
            "c280fcca9d56e8f62757ee05ee17cec953bf09958e78d92ab50804d9d23b15a98a0a4ff6de421e78f73cd4874524e8d17efb2a21cccb92d9fac42407c2f3b031"
          ]
        ],
        [
          "b45fd52573e8e6bfe0aff82fb228e887fdd92210fe0952ae65a59080fec7e529",
          [
            "5625ce8a09a441111fc1fc1e8491a5ba0dd84a0bdc73c4c3e284321c64f1dfbd9a863725f0da7a2e99f014c3d10c6eb207f7d5bd06fd8fcacbc31a6d040bde7a"
          ]
        ]
        ]
    },
    "id": 1
}' | jq


curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "getsubnetcheckpoint",
    "params": {
        "subnet_id": "/b4/t420fluns5gbn747yx4niyqwmfvsessium7psci7w7lld7omo3n2hrutp3ayv2y"
    },
    "id": 1
}' | jq

# get a specific subnet checkpoint
curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer asda123123jhaskjdhgbjsjhdj" \
-d '{
    "jsonrpc": "2.0",
    "method": "getsubnetcheckpoint",
    "params": {
        "subnet_id": "/b4/t420fluns5gbn747yx4niyqwmfvsessium7psci7w7lld7omo3n2hrutp3ayv2y",
        "number": 1
    },
    "id": 1
}' | jq


# get rewarded collaterals for all validators for a past snapshot
curl -X POST http://localhost:3030/api \
-H "Content-Type: application/json" \
-H "Authorization: Bearer validator1_auth_token" \
-d '{
    "jsonrpc": "2.0",
    "method": "getrewardedcollaterals",
    "params": {
        "snapshot": 43
    },
    "id": 1
}' | jq

```