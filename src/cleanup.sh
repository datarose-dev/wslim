# Runs only in a disposable distro for "clean"; "preview" never deletes.
set -eu
mode=$1
selected=,$2,

has_group() {
    case "$selected" in *,"$1",*) return 0 ;; *) return 1 ;; esac
}

# Never clean a cache reached through a symlink (it could point outside WSL).
has_symlink_component() {
    path=$1
    while [ "$path" != / ]; do
        [ -L "$path" ] && return 0
        path=${path%/*}
        [ -n "$path" ] || path=/
    done
    return 1
}

# Keep the scan on Linux filesystems. Do not traverse Windows mounts.
if has_group node || has_group php || has_group rust; then
for root in /home /root /workspace /workspaces /srv /opt; do
    [ -d "$root" ] || continue
    find "$root" -xdev -type d \( -name node_modules -o -name vendor -o -name target \) \
        -prune -exec /bin/sh -c '
            mode=$1; selected=$2; shift 2
            for dir do
                parent=${dir%/*}
                case ${dir##*/} in
                    node_modules) group=node; marker=package.json ;;
                    vendor) group=php; marker=composer.json ;;
                    target) group=rust; marker=Cargo.toml ;;
                esac
                case ",$selected," in *,"$group",*) ;; *) continue ;; esac
                [ -f "$parent/$marker" ] || continue
                printf "%s\n" "$dir"
                if [ "$mode" = clean ]; then
                    rm -rf -- "$dir"
                fi
            done
        ' wslim "$mode" "$2" {} +
done
fi

if has_group caches; then
    # Preserve executables, credentials and config (including ~/.cargo/bin).
    for home in /root /home/*; do
        [ -d "$home" ] || continue
        for cache in \
            .npm/_cacache \
            .cache/composer \
            .composer/cache \
            .local/share/pnpm/store \
            .cargo/registry/cache \
            .cargo/registry/src \
            .cargo/registry/index \
            .cargo/git/checkouts \
            .cargo/git/db; do
            dir="$home/$cache"
            [ -e "$dir" ] || [ -L "$dir" ] || continue
            has_symlink_component "$dir" && continue
            printf "%s\n" "$dir"
            if [ "$mode" = clean ]; then
                rm -rf -- "$dir"
            fi
        done
    done
fi
