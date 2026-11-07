#!/bin/sh
# Lance le serveur SystemOne fourni avec Clef (clef_mlx.py serve) sur 127.0.0.1.
# Usage : banc/clef-serve.sh <dossier du modèle> [port]
# Aucun code Python ici : tout le pont est dans le fichier fourni avec le modèle.
# --no-truncate : un instantané trop long échoue (413), au lieu d'être coupé en
# silence, car le modèle doit voir ce que l'utilisateur a vu. Arrêt : Ctrl-C.
#
# Refus avant tout chargement : un modèle trop gros pour cette machine, et mémoire
# insuffisante. Ces seuils ne se règlent pas.
set -eu

MAX_WEIGHTS_GB=20
MIN_FREE_GB=9
MIN_SWAP_FREE_MB=2048

model="${1:?dossier du modèle requis}"
port="${2:-8000}"
venv="${CS_BANC_VENV:-$HOME/.local/state/cc-sessions/clef-venv}"

case "$port" in
  ''|*[!0-9]*) echo "clef-serve : port invalide." >&2; exit 1 ;;
esac
# Clef complet n'a pas `clef_mlx.py` : le message doit dire la vraie raison.
[ -d "$model" ] || { echo "clef-serve : dossier absent : refus." >&2; exit 1; }

# Taille réelle de tout le dossier, liens suivis (disposition du cache Hugging Face),
# quel que soit le format. Une mesure impossible vaut un refus.
real_model=$(cd -P "$model" && pwd -P)
total_bytes=$(/usr/bin/find -L "$real_model" -type f -exec /usr/bin/stat -L -f %z {} + | /usr/bin/awk '{n+=$1} END {printf "%.0f", n}')
case "$total_bytes" in
  ''|0|*[!0-9]*) echo "clef-serve : taille du modèle impossible à mesurer : refus." >&2; exit 1 ;;
esac
weights_gb=$((total_bytes / 1000000000))
if [ "$weights_gb" -gt "$MAX_WEIGHTS_GB" ]; then
  echo "clef-serve : ${weights_gb} Go de poids : modèle trop gros pour cette machine, refus." >&2
  exit 1
fi

# Le pic mémoire suit la taille des poids : Clef Flash 4 bits, 5,8 Go, monte à 8,6 Go.
need_gb=$((weights_gb * 3 / 2 + 1))
[ "$need_gb" -ge "$MIN_FREE_GB" ] || need_gb=$MIN_FREE_GB
pages=$(vm_stat | /usr/bin/awk '/Pages (free|inactive|speculative)/ {gsub(/\./,"",$NF); n+=$NF} END {print n}')
page_size=$(/usr/sbin/sysctl -n hw.pagesize)
free_gb=$((pages * page_size / 1073741824))
swap_free_mb=$(/usr/sbin/sysctl -n vm.swapusage | /usr/bin/sed -E 's/.*free = ([0-9]+).*/\1/')
if [ "$free_gb" -lt "$need_gb" ] || [ "$swap_free_mb" -lt "$MIN_SWAP_FREE_MB" ]; then
  echo "clef-serve : ${free_gb} Go disponibles et ${swap_free_mb} Mo de swap libres, il faut ${need_gb} Go et ${MIN_SWAP_FREE_MB} Mo : refus." >&2
  exit 1
fi

[ -f "$real_model/clef_mlx.py" ] || { echo "clef-serve : clef_mlx.py absent de ce dossier : refus." >&2; exit 1; }

# Valeurs passées par l'environnement : un chemin ne peut pas devenir une commande.
# Versions testées d'après la fiche du modèle. mlx-vlm de nixpkgs est trop ancien.
export CLEF_MODEL="$real_model" CLEF_PORT="$port" CLEF_VENV="$venv"
exec nix-shell -p uv python312 --run '
  set -eu
  [ -x "$CLEF_VENV/bin/python" ] || uv venv --python python3.12 "$CLEF_VENV"
  uv pip install --quiet --python "$CLEF_VENV/bin/python" "mlx==0.32.3" "mlx-lm==0.32.0" "mlx-vlm==0.7.4"
  cd "$CLEF_MODEL"
  HF_HUB_OFFLINE=1 exec "$CLEF_VENV/bin/python" -I clef_mlx.py serve --host 127.0.0.1 --port "$CLEF_PORT" --no-truncate
'
