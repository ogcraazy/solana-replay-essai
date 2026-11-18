# Provenance des définitions du flux

`geyser.proto` et `solana-storage.proto` sont des copies conformes, octet pour octet, des
définitions du projet Yellowstone gRPC, le protocole que suivent les fournisseurs de flux
Solana.

| | |
|---|---|
| Dépôt | `https://github.com/rpcpool/yellowstone-grpc` |
| Version | `v17.0.1+solana.4.4.1` |
| Commit | `c1371f80ea7e3288c22019037673c7d73734462a` |
| Répertoire d'origine | `yellowstone-grpc-proto/proto/` |
| Licence | Apache-2.0 (`LICENSE_APACHE2`, copié du même répertoire) |

Leurs empreintes SHA-256 sont dans `../EMPREINTES.sha256`, que la recette contrôle avant de
s'en servir. Le module Python de ces définitions n'est pas gardé ici : la recette le produit
à chaque exécution, par le `protoc` de `grpcio-tools`, aux versions et aux empreintes de
fichiers fixées dans `../dependances.txt`.
