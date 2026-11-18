# Empreintes d'un flux

- `empreintes.py` : lit un flux de transactions et d'en-têtes de blocs au format Yellowstone
  gRPC et en rend des sommes d'empreintes, par tranche de slots et pour la plage entière.
  Deux sources qui rendent les mêmes transactions rendent les mêmes sommes ; ses définitions
  sont en tête du fichier. Il ne dépend que de Python et de la bibliothèque `protobuf`.
- `definitions/` : les définitions officielles du flux, copiées telles quelles (voir
  `definitions/PROVENANCE.md`).
- `dependances.txt` : les bibliothèques Python de la recette, versions et empreintes de
  fichiers fixées (produit depuis `dependances.in` par `uv pip compile --generate-hashes`).
- `EMPREINTES.sha256` : les empreintes des trois fichiers ci-dessus dont dépend le résultat.
