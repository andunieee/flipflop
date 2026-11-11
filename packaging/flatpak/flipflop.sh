#!/bin/sh
# The sandbox points XDG_DATA_HOME at ~/.var/app/dev.flipflop.app; keep the identity in ~/.local/share/flipflop
# instead, where the command line client (and `flipflop gui`) look for it.
export FLIPFLOP_DIR="${FLIPFLOP_DIR:-$HOME/.local/share/flipflop}"
exec /app/lib/flipflop/bin/flipflop-gui "$@"
