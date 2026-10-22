#!/bin/sh
# Standalone: /bin/sh tests/cleanup-fixtures.sh
# Windows: & 'C:\Program Files\Git\bin\bash.exe' tests/cleanup-fixtures.sh
# No production overrides: assemble the production script with sandbox literals.
set -eu

repository=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
mkdir -p "$repository/tests/cleanup-fixtures"
sandbox=$(mktemp -d "$repository/tests/cleanup-fixtures/run.XXXXXX")
trap 'rm -rf -- "$sandbox"' 0
trap 'exit 1' 1 2 3 15
# Git Bash must create symlinks understood by its POSIX tools, not directory copies.
MSYS="${MSYS:-} winsymlinks:lnk"
export MSYS
cases=0
tab=$(printf '\t')

fail() {
    printf 'FAIL: %s: %s\n' "$case_name" "$1" >&2
    exit 1
}

shell_literal() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

sed_literal() {
    shell_literal "$1" | sed 's/[\\&|]/\\&/g'
}

start_case() {
    case_name=$1
    case_dir=$sandbox/$case_name
    tree=$case_dir/tree
    mkdir -p "$tree" "$case_dir/bin"
    printf '1 0 8:1 / / rw - ext4 /dev/root rw\n' > "$case_dir/mountinfo"
    : > "$case_dir/calls"
    : > "$case_dir/expected"
    export FIXTURE_CASE=$case_dir
    export FIXTURE_FAIL_RM=0
    export FIXTURE_AFTER_RM=
    export FIXTURE_SWAP_PATH=
    printf '0\n' > "$case_dir/reads"
    sed \
        -e "s|\"/proc/self/mountinfo\"|$(sed_literal "$case_dir/mountinfo")|g" \
        -e "s|\"/home\"|$(sed_literal "$tree/home")|g" \
        -e "s|\"/home/\"|$(sed_literal "$tree/home/")|g" \
        -e "s|\"/root\"|$(sed_literal "$tree/root")|g" \
        -e "s|\"/workspace\"|$(sed_literal "$tree/workspace")|g" \
        -e "s|\"/workspaces\"|$(sed_literal "$tree/workspaces")|g" \
        -e "s|\"/srv\"|$(sed_literal "$tree/srv")|g" \
        -e "s|\"/opt\"|$(sed_literal "$tree/opt")|g" \
        "$repository/src/cleanup.sh" > "$case_dir/cleanup.sh"
    sed '/^# Production entry point\./,$d' "$case_dir/cleanup.sh" > "$case_dir/helpers.sh"
    /bin/sh -n "$case_dir/cleanup.sh"
    cat > "$case_dir/bin/rm" <<'EOF'
#!/bin/sh
set -eu
[ "$#" -eq 3 ] && [ "$1" = -rf ] && [ "$2" = -- ] || exit 90
case "$3" in "$FIXTURE_CASE/tree/"*) ;; *) exit 91 ;; esac
printf '%s\n' "$3" >> "$FIXTURE_CASE/calls"
[ "$FIXTURE_FAIL_RM" = 0 ] || exit 73
/bin/rm "$@" || exit "$?"
if [ -n "$FIXTURE_AFTER_RM" ]; then
    cat "$FIXTURE_AFTER_RM" > "$FIXTURE_CASE/mountinfo"
fi
EOF
    chmod +x "$case_dir/bin/rm"
    cat > "$case_dir/bin/cat" <<'EOF'
#!/bin/sh
set -eu
if [ -n "$FIXTURE_SWAP_PATH" ] && [ "$#" -eq 1 ] && [ "$1" = "$FIXTURE_CASE/mountinfo" ]; then
    reads=$(/bin/cat "$FIXTURE_CASE/reads")
    reads=$((reads + 1))
    printf '%s\n' "$reads" > "$FIXTURE_CASE/reads"
    if [ "$reads" -eq 2 ]; then
        /bin/mv "$FIXTURE_SWAP_PATH" "$FIXTURE_CASE/tree/outside/original"
        /bin/ln -s "$FIXTURE_CASE/tree/outside/replacement" "$FIXTURE_SWAP_PATH"
    fi
fi
/bin/cat "$@"
EOF
    chmod +x "$case_dir/bin/cat"
}

project() {
    mkdir -p "$tree/$1/$2"
    : > "$tree/$1/$3"
    printf 'keep unless eligible\n' > "$tree/$1/$2/payload"
}

expect_path() {
    printf '%s\n' "$tree/$1" >> "$case_dir/expected"
}

retain_path() {
    [ -e "$tree/$1" ] || [ -L "$tree/$1" ] || fail "protected path removed: $1"
}

mount_path() {
    encoded=$(printf '%s' "$1" | sed -e 's/\\/\\134/g' -e 's/ /\\040/g' -e "s/$tab/\\\\011/g")
    # Deliberately use the root device ID: bind/file mounts must still be protected.
    printf '2 1 8:1 / %s rw shared:1 - %s %s %s\n' "$encoded" "${2:-ext4}" \
        "${3:-none}" "${4:-rw}" >> "$case_dir/mountinfo"
}

run_cleanup() {
    PATH="$case_dir/bin:$PATH" /bin/sh "$case_dir/cleanup.sh" "$1" "$2" \
        > "$case_dir/$1.stdout" 2> "$case_dir/$1.stderr"
}

assert_expected() {
    sort "$case_dir/expected" > "$case_dir/expected.sorted"
    sort "$case_dir/$1.stdout" > "$case_dir/$1.sorted"
    cmp -s "$case_dir/expected.sorted" "$case_dir/$1.sorted" || fail "$1 paths differ from expected"
}

assert_parity() {
    run_cleanup preview "$1" || fail 'preview failed'
    assert_expected preview
    [ ! -s "$case_dir/calls" ] || fail 'preview called rm'
    while IFS= read -r path; do
        [ -e "$path" ] || fail 'preview removed a candidate'
    done < "$case_dir/expected"
    run_cleanup clean "$1" || fail 'clean failed'
    assert_expected clean
    cmp -s "$case_dir/preview.stdout" "$case_dir/clean.stdout" || fail 'preview/clean protocol differs'
    while IFS= read -r path; do
        [ ! -e "$path" ] && [ ! -L "$path" ] || fail 'clean retained an eligible candidate'
    done < "$case_dir/expected"
    sort "$case_dir/calls" > "$case_dir/calls.sorted"
    cmp -s "$case_dir/expected.sorted" "$case_dir/calls.sorted" || fail 'rm calls differ from successful paths'
    cases=$((cases + 1))
}

assert_rejected_metadata() {
    for action in preview clean; do
        if run_cleanup "$action" node,caches; then
            fail "$action accepted invalid metadata"
        fi
        [ ! -s "$case_dir/$action.stdout" ] || fail 'invalid metadata emitted success paths'
        [ -s "$case_dir/$action.stderr" ] || fail 'invalid metadata omitted stderr error'
        [ ! -s "$case_dir/calls" ] || fail 'invalid metadata called rm'
        retain_path home/alice/project/node_modules/payload
        retain_path home/alice/.npm/_cacache/payload
    done
    cases=$((cases + 1))
}

probe_mount() {
    /bin/sh -c '. "$1"; load_mounts || exit 9; mount_blocks_path "$2" "$3"' \
        fixtures "$case_dir/helpers.sh" "$1" "$2"
}

# Selected candidates prune; unselected groups and markerless names do not.
start_case nesting_hidden_spaces
project 'home/alice/vendor' node_modules package.json
: > "$tree/home/alice/composer.json"
project 'home/alice/target/deep project' node_modules package.json
project 'home/alice/.hidden project' node_modules package.json
project 'home/alice/..hidden project' node_modules package.json
project 'workspace/pruned' node_modules package.json
project 'workspace/pruned/node_modules/nested' node_modules package.json
expect_path home/alice/vendor/node_modules
expect_path 'home/alice/target/deep project/node_modules'
expect_path 'home/alice/.hidden project/node_modules'
expect_path 'home/alice/..hidden project/node_modules'
expect_path workspace/pruned/node_modules
assert_parity node
retain_path home/alice/vendor
retain_path home/alice/target

# Preserve the entire cache allowlist and each project marker policy.
start_case all_groups
project home/alice/node node_modules package.json
project home/alice/php vendor composer.json
project home/alice/rust target Cargo.toml
expect_path home/alice/node/node_modules
expect_path home/alice/php/vendor
expect_path home/alice/rust/target
for cache in .npm/_cacache .cache/composer .composer/cache .local/share/pnpm/store \
    .cargo/registry/cache .cargo/registry/src .cargo/registry/index .cargo/git/checkouts .cargo/git/db; do
    mkdir -p "$tree/root/$cache"
    : > "$tree/root/$cache/payload"
    expect_path "root/$cache"
done
mkdir -p "$tree/root/.cargo/bin"
: > "$tree/root/.cargo/bin/tool"
assert_parity node,php,rust,caches
retain_path root/.cargo/bin/tool
retain_path home/alice/node/package.json
retain_path home/alice/php/composer.json
retain_path home/alice/rust/Cargo.toml

start_case mounted_scan_root
project home/alice/project node_modules package.json
project workspace/project node_modules package.json
mount_path "$tree/home" drvfs
expect_path workspace/project/node_modules
assert_parity node
retain_path home/alice/project/node_modules/payload

# WSL2's 9p super-options contain a verbatim DrvFs aname Windows path.
# Opaque whitespace, backslashes and separator-like text must not become fields
# or protective paths. Unrelated project/cache candidates remain eligible.
for windows_path in backslashes spaces tabs separator_like; do
    start_case "wsl2_drvfs_$windows_path"
    project workspace/host-project node_modules package.json
    project home/alice/project node_modules package.json
    mkdir -p "$tree/home/alice/.npm/_cacache"
    : > "$tree/home/alice/.npm/_cacache/payload"
    case "$windows_path" in
        backslashes) path='C:\data' ;;
        spaces) path='C:/Program Files' ;;
        tabs) path="C:/Data${tab}Files" ;;
        separator_like) path='C:/Program - Files' ;;
    esac
    mount_path "$tree/workspace" 9p 'C:\134' \
        "rw,dirsync,aname=drvfs;path=$path;uid=1000;gid=1000,msize=65536,trans=fd"
    expect_path home/alice/project/node_modules
    expect_path home/alice/.npm/_cacache
    assert_parity node,caches
    retain_path workspace/host-project/node_modules/payload
    [ ! -s "$case_dir/preview.stderr" ] && [ ! -s "$case_dir/clean.stderr" ] || fail 'valid DrvFs metadata emitted errors'
done

# Previously overspecified failures: options may span multiple words, and the
# mount source is not a protective path whose escapes should be decoded.
for opaque in extra_tail source_escape; do
    start_case "opaque_$opaque"
    project home/alice/project node_modules package.json
    case "$opaque" in
        extra_tail) mount_path "$tree/workspace" 9p none 'rw,aname=drvfs;path=C:/Program Files' ;;
        source_escape) mount_path "$tree/workspace" ext4 'source\x' rw ;;
    esac
    expect_path home/alice/project/node_modules
    assert_parity node
    [ ! -s "$case_dir/preview.stderr" ] && [ ! -s "$case_dir/clean.stderr" ] || fail 'opaque source/options emitted errors'
done

start_case mounted_ancestor_component_boundary
project home/a/project node_modules package.json
project home/abc/project node_modules package.json
mount_path "$tree/home/a"
expect_path home/abc/project/node_modules
assert_parity node
retain_path home/a/project/node_modules/payload

start_case mounted_candidates
project srv/node node_modules package.json
project srv/php vendor composer.json
project srv/rust target Cargo.toml
for candidate in srv/node/node_modules srv/php/vendor srv/rust/target; do
    mount_path "$tree/$candidate"
done
assert_parity node,php,rust
retain_path srv/node/node_modules/payload
retain_path srv/php/vendor/payload
retain_path srv/rust/target/payload

# Unsafe outer candidates still expose safe nested candidates. File mounts count.
start_case descendant_bind_and_file_mounts
project opt/project vendor composer.json
mkdir -p "$tree/opt/project/vendor/mounted"
: > "$tree/opt/project/vendor/mounted/payload"
project opt/project/vendor/safe node_modules package.json
mount_path "$tree/opt/project/vendor/mounted" tmpfs
project workspace/project target Cargo.toml
: > "$tree/workspace/project/target/built.o"
mount_path "$tree/workspace/project/target/built.o"
expect_path opt/project/vendor/safe/node_modules
assert_parity node,php,rust
retain_path opt/project/vendor/payload
retain_path opt/project/vendor/mounted/payload
retain_path workspace/project/target/built.o

start_case mounted_caches
mkdir -p "$tree/home/alice/.npm/_cacache" "$tree/home/alice/.cache/composer/subdir" \
    "$tree/home/alice/.cargo/registry/cache" "$tree/home/alice/.composer/cache"
: > "$tree/home/alice/.npm/_cacache/payload"
: > "$tree/home/alice/.cargo/registry/cache/file"
mount_path "$tree/home/alice/.npm/_cacache"
mount_path "$tree/home/alice/.cache/composer/subdir"
mount_path "$tree/home/alice/.cargo/registry/cache/file"
expect_path home/alice/.composer/cache
assert_parity caches
retain_path home/alice/.npm/_cacache/payload
retain_path home/alice/.cache/composer/subdir
retain_path home/alice/.cargo/registry/cache/file

start_case symlink_components
project outside/root-project node_modules package.json
mkdir -p "$tree/home/alice/project" "$tree/outside/cache" "$tree/outside/pnpm/store" \
    "$tree/outside/home/.npm/_cacache" "$tree/home/alice/.local/share" "$tree/home/alice/.npm"
ln -s "$tree/outside" "$tree/root"
ln -s "$tree/outside" "$tree/home/alice/link"
ln -s "$tree/outside/cache" "$tree/home/alice/project/node_modules"
: > "$tree/home/alice/project/package.json"
ln -s "$tree/outside/cache" "$tree/home/alice/.cache"
ln -s "$tree/outside/pnpm" "$tree/home/alice/.local/share/pnpm"
ln -s "$tree/outside/cache" "$tree/home/alice/.npm/_cacache"
ln -s "$tree/outside/home" "$tree/home/linked"
[ -L "$tree/root" ] && [ -L "$tree/home/alice/.cache" ] || fail 'host does not support fixture symlinks'
project srv/safe node_modules package.json
expect_path srv/safe/node_modules
assert_parity node,caches
retain_path outside/root-project/node_modules/payload
retain_path outside/home/.npm/_cacache
retain_path home/alice/project/node_modules
retain_path home/alice/.cache
retain_path home/alice/.local/share/pnpm/store

# Change a path component during the candidate's metadata refresh. The second
# symlink check must reject both project and cache candidates without deletion.
for swap in project cache; do
    start_case "symlink_refresh_$swap"
    mkdir -p "$tree/outside/replacement"
    if [ "$swap" = project ]; then
        project home/alice/project node_modules package.json
        project outside/replacement node_modules package.json
        FIXTURE_SWAP_PATH=$tree/home/alice/project
        group=node
    else
        mkdir -p "$tree/home/alice/.npm/_cacache" "$tree/outside/replacement/_cacache"
        : > "$tree/home/alice/.npm/_cacache/payload"
        : > "$tree/outside/replacement/_cacache/payload"
        FIXTURE_SWAP_PATH=$tree/home/alice/.npm
        group=caches
    fi
    export FIXTURE_SWAP_PATH
    run_cleanup clean "$group" || fail 'symlink refresh fixture failed'
    [ -L "$FIXTURE_SWAP_PATH" ] || fail 'fixture did not swap the path component'
    [ ! -s "$case_dir/clean.stdout" ] || fail 'swapped symlink emitted a success path'
    [ ! -s "$case_dir/calls" ] || fail 'swapped symlink called rm'
    if [ "$swap" = project ]; then
        retain_path outside/original/node_modules/payload
        retain_path outside/replacement/node_modules/payload
    else
        retain_path outside/original/_cacache/payload
        retain_path outside/replacement/_cacache/payload
    fi
    cases=$((cases + 1))
done

# Spaces are exercised on disk. Tabs/backslashes use guards directly because
# Windows cannot represent all Linux filenames in a fixture directory.
start_case mount_escapes
project 'home/alice/mounted project' node_modules package.json
project 'home/alice/safe project' node_modules package.json
mount_path "$tree/home/alice/mounted project"
mount_path "/virtual/with${tab}tab"
mount_path '/virtual/with\backslash'
mount_path '/virtual/\040'
probe_mount "/virtual/with${tab}tab/child" scan || fail 'tab escape was not decoded'
probe_mount '/virtual/with\backslash/child' scan || fail 'backslash escape was not decoded'
probe_mount '/virtual/\040/child' scan || fail 'mount escape decoded recursively'
if probe_mount '/virtual/ /child' scan; then fail 'literal backslash became a space'; fi
probe_mount /virtual delete || fail 'escaped descendant mount not protected'
if probe_mount /virtualx delete; then fail 'mount component boundary ignored'; fi
expect_path 'home/alice/safe project/node_modules'
assert_parity node
retain_path 'home/alice/mounted project/node_modules/payload'
cases=$((cases + 1))

# Reject newline-bearing candidates before any filesystem access or stdout emit.
start_case newline_candidate
newline='
'
if PATH="$case_dir/bin:$PATH" /bin/sh -c \
    '. "$1"; mode=clean; load_mounts || exit 1; process_candidate "$2"' \
    fixtures "$case_dir/helpers.sh" "$tree/home/new${newline}line/node_modules" \
    > "$case_dir/clean.stdout" 2> "$case_dir/clean.stderr"; then
    fail 'newline candidate accepted'
else
    [ "$?" -eq 2 ] || fail 'newline candidate returned wrong status'
fi
[ ! -s "$case_dir/clean.stdout" ] || fail 'newline candidate emitted a success path'
[ -s "$case_dir/clean.stderr" ] || fail 'newline candidate omitted rejection error'
[ ! -s "$case_dir/calls" ] || fail 'newline candidate called rm'
cases=$((cases + 1))

# All metadata failures must happen before either projects or caches are touched.
for invalid in missing empty directory short separator id device options missing_super_options filesystem optional \
    relative noncanonical unsupported root_unsupported incomplete_escape newline_mount newline_root \
    missing_root late_invalid raw_tab trailing_blank; do
    start_case "metadata_$invalid"
    project home/alice/project node_modules package.json
    mkdir -p "$tree/home/alice/.npm/_cacache"
    : > "$tree/home/alice/.npm/_cacache/payload"
    case "$invalid" in
        missing) rm -- "$case_dir/mountinfo" ;;
        empty) : > "$case_dir/mountinfo" ;;
        directory) rm -- "$case_dir/mountinfo"; mkdir "$case_dir/mountinfo" ;;
        short) printf '1 0 8:1 / / rw - ext4\n' > "$case_dir/mountinfo" ;;
        separator) printf '1 0 8:1 / / rw shared:1 ext4 none rw opaque tail\n' > "$case_dir/mountinfo" ;;
        id) printf '1:2 0 8:1 / / rw - ext4 none rw\n' > "$case_dir/mountinfo" ;;
        device) printf '1 0 8:1:2 / / rw - ext4 none rw\n' > "$case_dir/mountinfo" ;;
        options) printf '1 0 8:1 / / bad - ext4 none rw\n' > "$case_dir/mountinfo" ;;
        missing_super_options) printf '1 0 8:1 / / rw - ext4 none\n' > "$case_dir/mountinfo" ;;
        filesystem) printf '1 0 8:1 / / rw - - none rw\n' > "$case_dir/mountinfo" ;;
        optional) printf '1 0 8:1 / / rw shared: - ext4 none rw\n' > "$case_dir/mountinfo" ;;
        relative) printf '2 1 8:1 / relative rw - ext4 none rw\n' >> "$case_dir/mountinfo" ;;
        noncanonical) printf '2 1 8:1 / /home/../root rw - ext4 none rw\n' >> "$case_dir/mountinfo" ;;
        unsupported) printf '%s\n' '2 1 8:1 / /virtual/\777 rw - ext4 none rw' >> "$case_dir/mountinfo" ;;
        root_unsupported) printf '%s\n' '2 1 8:1 /virtual/\777 /virtual rw - ext4 none rw' >> "$case_dir/mountinfo" ;;
        incomplete_escape) printf '%s\n' '2 1 8:1 / /virtual/\04 rw - ext4 none rw' >> "$case_dir/mountinfo" ;;
        newline_mount) printf '%s\n' '2 1 8:1 / /virtual/\012 rw - ext4 none rw' >> "$case_dir/mountinfo" ;;
        newline_root) printf '%s\n' '2 1 8:1 /virtual/\012 /virtual rw - ext4 none rw' >> "$case_dir/mountinfo" ;;
        missing_root) printf '2 1 8:1 / /virtual rw - ext4 none rw\n' > "$case_dir/mountinfo" ;;
        late_invalid) mount_path "$tree/opt"; printf 'bad record\n' >> "$case_dir/mountinfo" ;;
        raw_tab) printf '2 1 8:1 / /virtual%stab rw - ext4 none rw\n' "$tab" >> "$case_dir/mountinfo" ;;
        trailing_blank) printf '\n' >> "$case_dir/mountinfo" ;;
    esac
    assert_rejected_metadata
done

start_case none_without_metadata
project home/alice/project node_modules package.json
rm -- "$case_dir/mountinfo"
assert_parity none
retain_path home/alice/project/node_modules/payload

start_case rm_failure_propagates
project home/alice/a/deep node_modules package.json
project home/alice/z node_modules package.json
FIXTURE_FAIL_RM=1
export FIXTURE_FAIL_RM
run_cleanup preview node || fail 'failure fixture preview failed'
[ ! -s "$case_dir/calls" ] || fail 'preview called failing rm'
if run_cleanup clean node; then fail 'rm failure was hidden'; fi
[ ! -s "$case_dir/clean.stdout" ] || fail 'rm failure emitted a success path'
[ -s "$case_dir/clean.stderr" ] || fail 'rm failure omitted stderr error'
printf '%s\n' "$tree/home/alice/a/deep/node_modules" > "$case_dir/expected"
sort "$case_dir/calls" > "$case_dir/calls.sorted"
cmp -s "$case_dir/expected" "$case_dir/calls.sorted" || fail 'traversal continued after rm failure'
retain_path home/alice/a/deep/node_modules/payload
retain_path home/alice/z/node_modules/payload
cases=$((cases + 1))

for refresh in invalid mounted; do
    start_case "refresh_$refresh"
    project home/alice/a node_modules package.json
    project home/alice/z node_modules package.json
    if [ "$refresh" = invalid ]; then
        printf 'invalid after first deletion\n' > "$case_dir/after-mountinfo"
    else
        mount_path "$tree/home/alice/z/node_modules"
        cat "$case_dir/mountinfo" > "$case_dir/after-mountinfo"
        printf '1 0 8:1 / / rw - ext4 /dev/root rw\n' > "$case_dir/mountinfo"
    fi
    FIXTURE_AFTER_RM=$case_dir/after-mountinfo
    export FIXTURE_AFTER_RM
    if run_cleanup clean node; then
        [ "$refresh" = mounted ] || fail 'invalid refreshed metadata accepted'
    else
        [ "$refresh" = invalid ] || fail 'valid refreshed mountinfo rejected'
    fi
    expect_path home/alice/a/node_modules
    assert_expected clean
    retain_path home/alice/z/node_modules/payload
    cmp -s "$case_dir/expected" "$case_dir/calls" || fail 'second deletion was not guarded by refreshed metadata'
    cases=$((cases + 1))
done

printf 'cleanup fixtures: %s checks passed (sandbox only)\n' "$cases"
