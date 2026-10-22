# Runs only in a disposable distro for "clean"; "preview" never deletes.
set -eu

cleanup_error() {
    printf 'wslim cleanup: %s\n' "$1" >&2
}

has_group() {
    case "$selected" in *,"$1",*) return 0 ;; *) return 1 ;; esac
}

# Decode only the four escapes emitted by mountinfo, never shell expressions.
# Newlines cannot be represented by the stdout path protocol.
decode_mount_field() (
    encoded=$1
    decoded=
    tab=$(printf '\t')
    newline='
'
    while [ -n "$encoded" ]; do
        case "$encoded" in
            *\\*)
                decoded=$decoded${encoded%%\\*}
                encoded=${encoded#*\\}
                case "$encoded" in
                    040*) decoded="$decoded " ;;
                    011*) decoded=$decoded$tab ;;
                    012*) decoded=$decoded$newline ;;
                    134*) decoded="$decoded\\" ;;
                    *) cleanup_error 'Malformed or unsupported mountinfo escape'; return 1 ;;
                esac
                encoded=${encoded#???}
                ;;
            *) decoded=$decoded$encoded; encoded= ;;
        esac
    done
    case "$decoded" in
        *"$newline"*) cleanup_error 'Newline-bearing mountinfo fields are unsupported'; return 1 ;;
    esac
    printf '%s' "$decoded"
)

valid_mount_path() {
    case "$1" in
        /) return 0 ;;
        /*/|*//*|*/./*|*/../*|*/.|*/..) return 1 ;;
        /*) return 0 ;;
        *) return 1 ;;
    esac
}

# Emit every non-root mount, including bind mounts and mounts on regular files.
# Parse the entire snapshot before traversal so a bad later record fails closed.
parse_mountinfo() (
    set -f
    IFS=' '
    root_seen=false
    tab=$(printf '\t')
    while IFS= read -r record || [ -n "$record" ]; do
        case "$record" in
            '') cleanup_error 'Malformed mountinfo record'; return 1 ;;
        esac
        set -- $record
        [ "$#" -ge 10 ] || { cleanup_error 'Incomplete mountinfo record'; return 1; }
        for field in "$1" "$2" "$3" "$4" "$5" "$6"; do
            case "$field" in
                *"$tab"*) cleanup_error 'Malformed mountinfo field'; return 1 ;;
            esac
        done
        for id in "$1" "$2"; do
            case "$id" in
                ''|*[!0-9]*) cleanup_error 'Invalid mountinfo IDs'; return 1 ;;
            esac
        done
        case "$3" in
            *:*) major=${3%%:*}; minor=${3#*:} ;;
            *) cleanup_error 'Invalid mountinfo device'; return 1 ;;
        esac
        case "$major:$minor" in
            *[!0-9:]*|:*|*:) cleanup_error 'Invalid mountinfo device'; return 1 ;;
        esac
        case "$minor" in *:*) cleanup_error 'Invalid mountinfo device'; return 1 ;; esac
        mount_root=$4
        mountpoint=$5
        case "$mount_root" in *\\*) mount_root=$(decode_mount_field "$mount_root") || return 1 ;; esac
        case "$mountpoint" in *\\*) mountpoint=$(decode_mount_field "$mountpoint") || return 1 ;; esac
        if ! valid_mount_path "$mount_root" || ! valid_mount_path "$mountpoint"; then
            cleanup_error 'Invalid mountinfo path'
            return 1
        fi
        case "$6" in
            ro|rw|ro,*|rw,*) ;;
            *) cleanup_error 'Invalid mountinfo options'; return 1 ;;
        esac
        shift 6
        while [ "$#" -gt 0 ] && [ "$1" != - ]; do
            case "$1" in *"$tab"*) cleanup_error 'Invalid mountinfo optional field'; return 1 ;; esac
            tag=${1%%:*}
            case "$tag" in
                ''|*[!a-zA-Z0-9_]*|[0-9]*) cleanup_error 'Invalid mountinfo optional field'; return 1 ;;
            esac
            case "$1" in *:) cleanup_error 'Empty mountinfo optional value'; return 1 ;; esac
            shift
        done
        [ "$#" -ge 4 ] || { cleanup_error 'Invalid mountinfo separator or fields'; return 1; }
        [ "$2" != - ] || { cleanup_error 'Invalid mountinfo filesystem'; return 1; }
        case "$2:$3" in
            *"$tab"*) cleanup_error 'Invalid mountinfo filesystem or source'; return 1 ;;
        esac
        # Filesystem options are opaque: WSL2 9p prints DrvFs Windows paths
        # verbatim, including spaces, tabs and backslashes. Decode only the
        # protective root and mountpoint, never the source or options tail.
        if [ "$mountpoint" = / ]; then
            root_seen=true
        else
            printf '%s\n' "$mountpoint" || return 1
        fi
    done
    [ "$root_seen" = true ] || { cleanup_error 'Mountinfo has no root mount'; return 1; }
)

load_mounts() {
    [ -r "/proc/self/mountinfo" ] || { cleanup_error 'Cannot read mountinfo'; return 1; }
    # The sentinel preserves trailing newlines, including malformed blank records.
    mountinfo=$(cat "/proc/self/mountinfo" && printf '.') || { cleanup_error 'Cannot read mountinfo'; return 1; }
    mountinfo=${mountinfo%.}
    [ -n "$mountinfo" ] || { cleanup_error 'Mountinfo is empty'; return 1; }
    protected_mounts=$(printf '%s' "$mountinfo" | parse_mountinfo) || return 1
}

has_symlink_component() (
    path=$1
    while [ "$path" != / ]; do
        [ -L "$path" ] && return 0
        path=${path%/*}
        [ -n "$path" ] || path=/
    done
    return 1
)

mount_blocks_path() (
    path=$1
    relationship=$2
    while IFS= read -r mountpoint; do
        [ -n "$mountpoint" ] || continue
        case "$path" in "$mountpoint"|"$mountpoint"/*) return 0 ;; esac
        if [ "$relationship" = delete ]; then
            case "$mountpoint" in "$path"|"$path"/*) return 0 ;; esac
        fi
    done <<EOF
$protected_mounts
EOF
    return 1
)

safe_path() {
    case "$1" in
        *'
'*) cleanup_error 'Newline-bearing cleanup paths are unsupported'; return 1 ;;
    esac
    has_symlink_component "$1" && return 1
    mount_blocks_path "$1" "$2" && return 1
    return 0
}

selected_project_candidate() (
    dir=$1
    parent=${dir%/*}
    case ${dir##*/} in
        node_modules) group=node; marker=package.json ;;
        vendor) group=php; marker=composer.json ;;
        target) group=rust; marker=Cargo.toml ;;
        *) return 1 ;;
    esac
    has_group "$group" && [ -f "$parent/$marker" ]
)

# Status 2 means protected, not an error. Only a successful eligible candidate
# may prune traversal. Refresh both modes to keep their eligibility identical.
process_candidate() {
    dir=$1
    safe_path "$dir" delete || return 2
    load_mounts || return 1
    safe_path "$dir" delete || return 2
    if [ "$mode" = clean ]; then
        if ! rm -rf -- "$dir"; then
            cleanup_error "Cannot remove $dir"
            return 1
        fi
    fi
    printf '%s\n' "$dir" || return 1
}

# Recursive variables live in subshells; no find/-exec status can hide rm errors.
walk_directory() (
    dir=$1
    safe_path "$dir" scan || return 0
    [ -d "$dir" ] || return 0
    if selected_project_candidate "$dir"; then
        if process_candidate "$dir"; then
            return 0
        else
            status=$?
            [ "$status" -eq 2 ] || return "$status"
        fi
    fi
    # A refreshed snapshot can protect this directory; a path can also have
    # become a symlink while reading metadata. Do not enumerate it in that case.
    safe_path "$dir" scan || return 0
    for child in "$dir"/* "$dir"/.[!.]* "$dir"/..?*; do
        [ -d "$child" ] || continue
        walk_directory "$child" || return 1
    done
)

# Production entry point. Fixtures assemble a sandbox-only copy of this script.
mode=$1
selected=,$2,
case "$mode" in
    preview|clean) ;;
    *) cleanup_error 'Unknown cleanup mode'; exit 1 ;;
esac

if ! has_group node && ! has_group php && ! has_group rust && ! has_group caches; then
    exit 0
fi

# A snapshot reduces risk but cannot exclude concurrent mount or path swaps.
load_mounts || exit 1
if has_group node || has_group php || has_group rust; then
    for root in "/home" "/root" "/workspace" "/workspaces" "/srv" "/opt"; do
        walk_directory "$root" || exit 1
    done
fi

if has_group caches; then
    # Preserve executables, credentials and config (including ~/.cargo/bin).
    for home in "/root" "/home/"*; do
        safe_path "$home" scan || continue
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
            if process_candidate "$dir"; then
                :
            else
                status=$?
                [ "$status" -eq 2 ] || exit "$status"
            fi
        done
    done
fi
