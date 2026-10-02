#!/bin/bash
# User-level release installation and receipt-based removal. macOS Bash 3.2.
set -euo pipefail
fail() { printf 'code-view installer: %s\n' "$*" >&2; exit 1; }
repository=windsyu/codex-plugin
action=install
prefix=${HOME:?HOME must be set}/.local
version=latest
force=false
configure_path=true
if [[ ${0##*/} == code-view-uninstall ]]; then
  action=uninstall
  prefix=$(cd -P -- "$(dirname -- "$0")/.." && pwd)
fi
case ${1-} in install|uninstall) action=$1; shift ;; esac
while [[ $# -gt 0 ]]; do
  case $1 in
    --prefix|--version)
      [[ $# -ge 2 ]] || fail "$1 needs a value"
      case $1 in --prefix) prefix=$2 ;; --version) version=$2 ;; esac
      shift 2 ;;
    --force) force=true; shift ;;
    --no-path) configure_path=false; shift ;;
    -h|--help)
      printf '%s\n' 'Usage: bash install-macos.sh [install|uninstall] [--prefix ABSOLUTE_PATH]' \
        '       install: [--version v0.x.y] [--force] [--no-path]' \
        'Default: latest stable macOS arm64 release, ~/.local, managed zsh/bash PATH.' \
        'Uninstall keeps user history/configuration and refuses modified owned files.'
      exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done
[[ $(uname -s) == Darwin && $(uname -m) == arm64 ]] || fail 'requires macOS on Apple Silicon'
[[ $action != uninstall || ($force == false && $configure_path == true && $version == latest) ]] || fail 'uninstall only accepts --prefix'
valid_path() {
  case $1 in /*) ;; *) fail "absolute path required: $1" ;; esac
  case $1 in /|*//*|*/../*|*/..|*/./*|*/.|*$'\n'*|*$'\r'*|*$'\t'*) fail "unsupported path: $1" ;; esac
}
prefix=${prefix%/}
valid_path "$prefix"
[[ $prefix != *:* ]] || fail 'prefix cannot contain colon (PATH separator)'
check_directory() {
  local directory=$1
  while [[ $directory != / ]]; do
    [[ ! -L $directory ]] || fail "symbolic link: $directory"
    [[ ! -e $directory || -d $directory ]] || fail "not a directory: $directory"
    directory=$(dirname -- "$directory")
  done
}
check_file() {
  check_directory "$(dirname -- "$1")"
  [[ ! -L $1 ]] || fail "symbolic link: $1"
  [[ ! -e $1 || -f $1 ]] || fail "not a regular file: $1"
}
check_writable() {
  local directory
  check_file "$1"
  [[ ! -e $1 || -w $1 ]] || fail "not writable: $1"
  directory=$(dirname -- "$1")
  while [[ ! -e $directory ]]; do directory=$(dirname -- "$directory"); done
  [[ -w $directory && -x $directory ]] || fail "directory not writable: $directory"
}
check_directory "$prefix/bin"
check_directory "$prefix/share/code-view"
work=$(mktemp -d "${TMPDIR:-/tmp}/code-view-install.XXXXXX")
# macOS TMPDIR may start with the system /var alias. Only canonicalize our own
# newly-created private workspace; installation destinations still reject links.
work=$(cd -P -- "$work" && pwd)
lock=
temporary_files=()
created_directories=()
install_committing=false
cleanup() {
  local task_exit_status=$? temporary index
  if [[ $task_exit_status != 0 && $install_committing == true ]]; then rollback_install; fi
  for temporary in ${temporary_files[@]+"${temporary_files[@]}"}; do rm -f -- "$temporary" || true; done
  if [[ -n $lock ]]; then rmdir -- "$lock" || true; fi
  if [[ $task_exit_status != 0 ]]; then
    for ((index=${#created_directories[@]}-1; index>=0; index--)); do rmdir -- "${created_directories[index]}" 2>/dev/null || true; done
  fi
  rm -rf -- "$work"
  return "$task_exit_status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
hash_file() { shasum -a 256 -- "$1" | cut -d ' ' -f 1; }
read_text() {
  text=$(cat -- "$1"; printf '.')
  text=${text%.}
  printf '%s' "$text" | cmp -s -- - "$1" || fail "not a text file: $1"
}
resource=$prefix/share/code-view
receipt=$resource/.install-receipt
block_file=$resource/.install-path-block
profiles_file=$resource/.install-path-profiles
uninstaller=$prefix/bin/code-view-uninstall
owned_paths=()
owned_hashes=()
initial_paths=()
initial_hashes=()
initial_present=()
initial_receipt=false
profile_paths=()
profile_created=()
profile_existed=()
profile_applied=()
install_targets=()
install_prepared=()
install_original=()
install_new_hashes=()
install_committed=0
ensure_directory() {
  local directory=$1 index missing=()
  check_directory "$directory"
  while [[ ! -d $directory ]]; do
    missing[${#missing[@]}]=$directory
    directory=$(dirname -- "$directory")
  done
  for ((index=${#missing[@]}-1; index>=0; index--)); do
    mkdir -- "${missing[index]}"
    created_directories[${#created_directories[@]}]=${missing[index]}
  done
}
restore_file() {
  local original=$1 destination=$2 temporary
  (check_file "$destination") || return 1
  temporary=$(mktemp "$(dirname -- "$destination")/.code-view-restore.XXXXXX") || return 1
  temporary_files[${#temporary_files[@]}]=$temporary
  cp -p -- "$original" "$temporary" && cmp -s -- "$original" "$temporary" && mv -f -- "$temporary" "$destination"
}
rollback_install() {
  local index destination
  # Restore only states actually committed by this operation. Concurrent edits
  # are preserved and reported rather than replaced with an older snapshot.
  for ((index=0; index<${#profile_paths[@]}; index++)); do
    [[ ${profile_applied[index]-false} == true ]] || continue
    destination=${profile_paths[index]}
    if (check_file "$destination") && [[ -f $destination ]] && cmp -s -- "$work/profile-$index" "$destination"; then
      if [[ ${profile_existed[index]} == true ]]; then
        restore_file "$work/profile-original-$index" "$destination" || printf 'Rollback failed: %s\n' "$destination" >&2
      else rm -- "$destination" || printf 'Rollback failed: %s\n' "$destination" >&2; fi
    else printf 'Rollback preserved a changed profile: %s\n' "$destination" >&2; fi
  done
  for ((index=install_committed-1; index>=0; index--)); do
    destination=${install_targets[index]}
    if (check_file "$destination") && [[ -f $destination ]] && [[ $(hash_file "$destination") == "${install_new_hashes[index]}" ]]; then
      if [[ ${install_original[index]} == true ]]; then
        restore_file "$work/install-original-$index" "$destination" || printf 'Rollback failed: %s\n' "$destination" >&2
      else rm -- "$destination" || printf 'Rollback failed: %s\n' "$destination" >&2; fi
    else printf 'Rollback preserved a changed installation file: %s\n' "$destination" >&2; fi
  done
}
valid_relative() {
  case $1 in ''|/*|*//*|*/../*|*/..|*/./*|*/.|*$'\n'*|*$'\r'*|*$'\t'*) fail 'invalid receipt path' ;; esac
  case $1 in
    bin/code-view|bin/codex-view|bin/codex-observerd|bin/code-view-uninstall) ;;
    share/code-view/LICENSE|share/code-view/THIRD_PARTY_NOTICES.md|share/code-view/RELEASE.md|share/code-view/manifest.json|share/code-view/Cargo.lock|share/code-view/CHANGELOG.md|share/code-view/web/package-lock.json|share/code-view/vendor/vt100/LICENSE|share/code-view/vendor/vt100/PATCH.md|share/code-view/web/src/workbench/icons/LICENSE|share/code-view/web/src/workbench/icons/README.md|share/code-view/licenses/*|share/code-view/.install-path-block|share/code-view/.install-path-profiles) ;;
    *) fail 'receipt path outside managed installation files' ;;
  esac
}
register_file() {
  local relative=$1 digest=$2 index
  valid_relative "$relative"
  for ((index=0; index<${#owned_paths[@]}; index++)); do
    if [[ ${owned_paths[index]} == "$relative" ]]; then owned_hashes[index]=$digest; return; fi
  done
  owned_paths[${#owned_paths[@]}]=$relative
  owned_hashes[${#owned_hashes[@]}]=$digest
}
load_receipt() {
  local header digest relative extra seen=$'\n'
  check_file "$receipt"
  [[ -f $receipt ]] || return 0
  cp -p -- "$receipt" "$work/receipt-original"
  initial_receipt=true
  read_text "$work/receipt-original"
  {
    IFS= read -r header || fail 'empty install receipt'
    [[ $header == code-view-install-v1 ]] || fail 'unsupported install receipt'
    while IFS=$'\t' read -r digest relative extra || [[ -n $digest$relative$extra ]]; do
      [[ $digest =~ ^[0-9a-f]{64}$ && -n $relative && -z $extra ]] || fail 'invalid install receipt row'
      valid_relative "$relative"
      [[ $seen != *$'\n'"$relative"$'\n'* ]] || fail 'duplicate install receipt path'
      seen=$seen$relative$'\n'
      check_file "$prefix/$relative"
      if [[ -e $prefix/$relative ]]; then
        [[ $(hash_file "$prefix/$relative") == "$digest" ]] || fail "modified installed file; preserve or restore before retry: $prefix/$relative"
        initial_present[${#initial_present[@]}]=true
      else initial_present[${#initial_present[@]}]=false
      fi
      initial_paths[${#initial_paths[@]}]=$relative
      initial_hashes[${#initial_hashes[@]}]=$digest
      register_file "$relative" "$digest"
    done
  } < "$work/receipt-original"
  [[ ${#owned_paths[@]} -ge 6 ]] || fail 'incomplete install receipt'
  local required found entry
  for required in bin/code-view-uninstall share/code-view/.install-path-block share/code-view/.install-path-profiles; do
    found=false
    for entry in ${owned_paths[@]+"${owned_paths[@]}"}; do [[ $entry != "$required" ]] || found=true; done
    [[ $found == true && -f $prefix/$required ]] || fail 'missing installation management file'
  done
}
revalidate_installation() {
  local index destination
  check_file "$receipt"
  if [[ $initial_receipt == true ]]; then
    [[ -f $receipt ]] && cmp -s -- "$work/receipt-original" "$receipt" || fail 'install receipt changed during operation'
  else [[ ! -e $receipt ]] || fail 'another installation appeared during operation'; fi
  for ((index=0; index<${#initial_paths[@]}; index++)); do
    destination=$prefix/${initial_paths[index]}
    check_file "$destination"
    if [[ ${initial_present[index]} == true ]]; then
      [[ -f $destination && $(hash_file "$destination") == "${initial_hashes[index]}" ]] || fail "installed file changed during operation: $destination"
    else [[ ! -e $destination ]] || fail "installed file appeared during operation: $destination"; fi
  done
}
revalidate_owned_file() {
  local relative=$1 index
  for ((index=0; index<${#initial_paths[@]}; index++)); do
    [[ ${initial_paths[index]} == "$relative" ]] || continue
    check_file "$prefix/$relative"
    if [[ ${initial_present[index]} == true ]]; then
      [[ -f $prefix/$relative && $(hash_file "$prefix/$relative") == "${initial_hashes[index]}" ]] || fail "installed file changed before write: $prefix/$relative"
    else [[ ! -e $prefix/$relative ]] || fail "installed file appeared before write: $prefix/$relative"; fi
    return 0
  done
}
load_profiles() {
  local created profile extra seen=$'\n'
  [[ -f $receipt ]] || return 0
  read_text "$block_file"
  path_block=$text
  [[ $path_block == "$expected_block" ]] || fail 'PATH block does not match installation prefix'
  read_text "$profiles_file"
  while IFS=$'\t' read -r created profile extra || [[ -n $created$profile$extra ]]; do
    [[ ($created == 0 || $created == 1) && -z $extra ]] || fail 'invalid PATH profile record'
    valid_path "$profile"
    case $profile in */.zshrc|"$HOME/.bash_profile") ;; *) fail 'invalid shell profile path' ;; esac
    [[ $seen != *$'\n'"$profile"$'\n'* ]] || fail 'duplicate PATH profile record'
    seen=$seen$profile$'\n'
    profile_paths[${#profile_paths[@]}]=$profile
    profile_created[${#profile_created[@]}]=$created
  done < "$profiles_file"
}
printf -v quoted_bin '%q' "$prefix/bin"
printf -v expected_block '\n# >>> code-view PATH >>>\ncase ":$PATH:" in\n  *:%s:*) ;;\n  *) export PATH=%s:"$PATH" ;;\nesac\n# <<< code-view PATH <<<\n' "$quoted_bin" "$quoted_bin"
path_block=$expected_block
load_receipt
load_profiles
add_profile() {
  local profile=$1 entry
  valid_path "$profile"
  for entry in ${profile_paths[@]+"${profile_paths[@]}"}; do [[ $entry != "$profile" ]] || return 0; done
  profile_paths[${#profile_paths[@]}]=$profile
  if [[ -e $profile ]]; then profile_created[${#profile_created[@]}]=0; else profile_created[${#profile_created[@]}]=1; fi
}
plan_profiles() {
  local index profile without_begin without_end
  local begin_marker='# >>> code-view PATH >>>' end_marker='# <<< code-view PATH <<<'
  for ((index=0; index<${#profile_paths[@]}; index++)); do
    profile=${profile_paths[index]}
    check_writable "$profile"
    text=
    if [[ -f $profile ]]; then
      cp -p -- "$profile" "$work/profile-original-$index"
      read_text "$work/profile-original-$index"
      cmp -s -- "$work/profile-original-$index" "$profile" || fail "shell profile changed while taking snapshot: $profile"
      profile_existed[index]=true
    else profile_existed[index]=false; fi
    without_begin=${text//"$begin_marker"/}
    without_end=${text//"$end_marker"/}
    if [[ $text != "$without_begin" || $text != "$without_end" ]]; then
      [[ $((${#text}-${#without_begin})) == ${#begin_marker} && $((${#text}-${#without_end})) == ${#end_marker} && $text == *"$path_block"* ]] || fail "modified or duplicate PATH block: $profile"
      if [[ $action == uninstall ]]; then text=${text/"$path_block"/}; fi
    elif [[ $text == *'code-view PATH'* ]]; then
      fail "modified PATH markers: $profile"
    elif [[ $action == install && $configure_path == true ]]; then
      text=$text$path_block
    fi
    printf '%s' "$text" > "$work/profile-$index"
  done
}
apply_profiles() {
  local index profile temporary
  for ((index=0; index<${#profile_paths[@]}; index++)); do
    profile=${profile_paths[index]}
    check_writable "$profile"
    if [[ ${profile_existed[index]} == true ]]; then
      [[ -f $profile ]] && cmp -s -- "$work/profile-original-$index" "$profile" || fail "shell profile changed after planning: $profile"
      if cmp -s -- "$work/profile-$index" "$profile"; then continue; fi
    else [[ ! -e $profile ]] || fail "shell profile appeared after planning: $profile"; fi
    if [[ $action == uninstall && ${profile_created[index]} == 1 && ! -s $work/profile-$index ]]; then
      [[ ! -e $profile ]] || rm -- "$profile"
    else
      ensure_directory "$(dirname -- "$profile")"
      temporary=$(mktemp "$(dirname -- "$profile")/.code-view-path.XXXXXX")
      temporary_files[${#temporary_files[@]}]=$temporary
      # Preserve the permissions/ACL of an existing dotfile. Partial writes
      # affect only the new file, never the user's configuration.
      if [[ ${profile_existed[index]} == true ]]; then cp -p -- "$work/profile-original-$index" "$temporary"; fi
      cat -- "$work/profile-$index" > "$temporary"
      cmp -s -- "$work/profile-$index" "$temporary" || fail 'incomplete shell profile write'
      check_file "$profile"
      if [[ ${profile_existed[index]} == true ]]; then
        [[ -f $profile ]] && cmp -s -- "$work/profile-original-$index" "$profile" || fail "shell profile changed before commit: $profile"
      else [[ ! -e $profile ]] || fail "shell profile appeared before commit: $profile"; fi
      mv -f -- "$temporary" "$profile"
      profile_applied[index]=true
    fi
  done
}
acquire_lock() {
  ensure_directory "$prefix"
  [[ ! -e $prefix/.code-view-install-lock && ! -L $prefix/.code-view-install-lock ]] || fail 'another management operation or stale lock exists'
  mkdir -- "$prefix/.code-view-install-lock" || fail 'cannot acquire installation lock'
  lock=$prefix/.code-view-install-lock
}
if [[ $action == uninstall ]]; then
  [[ -f $receipt ]] || fail 'no quick-install receipt; use an explicit --force quick reinstall to register an existing release'
  plan_profiles
  acquire_lock
  revalidate_installation
  for relative in ${owned_paths[@]+"${owned_paths[@]}"}; do
    case $relative in bin/code-view-uninstall|share/code-view/.install-path-block|share/code-view/.install-path-profiles) continue ;; esac
    revalidate_owned_file "$relative"
    [[ ! -e $prefix/$relative ]] || rm -- "$prefix/$relative"
  done
  apply_profiles
  # Keep the receipt/helper until ordinary files and PATH removal have succeeded.
  for file in "$block_file" "$profiles_file" "$receipt" "$uninstaller"; do cp -- "$file" "$work/${file##*/}"; done
  if ! rm -- "$block_file" "$profiles_file" "$receipt" "$uninstaller"; then
    for file in "$block_file" "$profiles_file" "$receipt" "$uninstaller"; do
      [[ -e $file ]] || cp -- "$work/${file##*/}" "$file"
    done
    fail 'management cleanup failed; receipt retained for retry'
  fi
  # Only remove known empty resource directories. Unknown files remain intact.
  for relative in ${owned_paths[@]+"${owned_paths[@]}"}; do
    case $relative in share/code-view/*) directory=$(dirname -- "$prefix/$relative") ;; *) continue ;; esac
    while [[ $directory == "$resource"/* ]]; do
      rmdir -- "$directory" 2>/dev/null || break
      directory=$(dirname -- "$directory")
    done
  done
  rmdir -- "$resource" 2>/dev/null || true
  printf '%s\n' 'Uninstalled code-view. User history, Codex data and unrelated files were preserved.' \
    'Open a new terminal to refresh PATH.'
  exit 0
fi
[[ $version == latest || $version =~ ^v0\.[0-9]+\.[0-9]+$ ]] || fail 'version must be latest or v0.x.y'
command -v gh >/dev/null || fail 'install GitHub CLI (gh) first, then run gh auth login'
gh auth status --active --hostname github.com >/dev/null 2>&1 || fail 'run gh auth login with an account that can access the repository'
if [[ $version == latest ]]; then version=$(gh release view --repo "$repository" --json tagName --jq .tagName); fi
[[ $version =~ ^v0\.[0-9]+\.[0-9]+$ ]] || fail 'release did not return a supported version'
archive_name=code-view-${version#v}-aarch64-apple-darwin.tar.gz
bundle_name=${archive_name%.tar.gz}
gh release download "$version" --repo "$repository" --dir "$work" --pattern "$archive_name" --pattern SHA256SUMS
check_file "$work/$archive_name"
check_file "$work/SHA256SUMS"
[[ -f $work/$archive_name && -f $work/SHA256SUMS ]] || fail 'missing release downloads'
read_text "$work/SHA256SUMS"
[[ $text == "$(hash_file "$work/$archive_name")  $archive_name"$'\n' ]] || fail 'archive checksum verification failed'
tar -tzf "$work/$archive_name" > "$work/members"
while IFS= read -r member; do
  case $member in "$bundle_name"/|"$bundle_name"/*) ;; *) fail 'archive member outside release directory' ;; esac
  case $member in *//*|*/../*|*/..|*/./*|*/.|*$'\r'*|*$'\t'*) fail 'unsafe archive member' ;; esac
done < "$work/members"
tar -tvzf "$work/$archive_name" > "$work/member-types"
while IFS= read -r member; do case $member in -*) ;; d*) ;; *) fail 'archive contains link or special file' ;; esac; done < "$work/member-types"
tar -xzf "$work/$archive_name" -C "$work"
bundle=$work/$bundle_name
check_file "$bundle/install.sh"
[[ -f $bundle/install.sh ]] || fail 'missing package installer'
check_file "$bundle/CHECKSUMS.sha256"
(cd -- "$bundle" && shasum -a 256 -c --status CHECKSUMS.sha256) || fail 'package checksum verification failed'
for file in "$receipt" "$block_file" "$profiles_file" "$uninstaller"; do
  check_writable "$file"
  [[ ! -e $file || $force == true ]] || fail "already exists; use --force for explicit reinstall: $file"
  [[ ! -e $file || -f $receipt ]] || fail "unmanaged management-file conflict: $file"
done
if [[ $configure_path == true ]]; then
  add_profile "${ZDOTDIR:-$HOME}/.zshrc"
  add_profile "$HOME/.bash_profile"
fi
plan_profiles
# Fixed package-to-install mapping; never claim unknown files left by --force.
while IFS= read -r checksum_row; do
  digest=${checksum_row:0:64}
  relative=${checksum_row:66}
  [[ $digest =~ ^[0-9a-f]{64}$ && ${checksum_row:64:2} == '  ' ]] || fail 'invalid package checksum row'
  case $relative in
    install.sh) continue ;;
    bin/code-view|bin/codex-view|bin/codex-observerd) mapped=$relative ;;
    *) mapped=share/code-view/$relative ;;
  esac
  valid_relative "$mapped"
  register_file "$mapped" "$digest"
done < "$bundle/CHECKSUMS.sha256"
cp -- "$0" "$work/uninstaller"
printf '%s' "$path_block" > "$work/path-block"
: > "$work/path-profiles"
for ((index=0; index<${#profile_paths[@]}; index++)); do printf '%s\t%s\n' "${profile_created[index]}" "${profile_paths[index]}" >> "$work/path-profiles"; done
register_file bin/code-view-uninstall "$(hash_file "$work/uninstaller")"
register_file share/code-view/.install-path-block "$(hash_file "$work/path-block")"
register_file share/code-view/.install-path-profiles "$(hash_file "$work/path-profiles")"
printf '%s\n' code-view-install-v1 > "$work/receipt"
for ((index=0; index<${#owned_paths[@]}; index++)); do printf '%s\t%s\n' "${owned_hashes[index]}" "${owned_paths[index]}" >> "$work/receipt"; done
acquire_lock
revalidate_installation
stage=$work/staged-prefix
# The legacy installer validates and copies into our private stage. A failed
# package copy cannot change an existing installation or its receipt.
/bin/sh "$bundle/install.sh" --prefix "$stage" > "$work/package-install.log"
cp -- "$work/uninstaller" "$stage/bin/code-view-uninstall"
chmod 755 "$stage/bin/code-view-uninstall"
cp -- "$work/path-block" "$stage/share/code-view/.install-path-block"
cp -- "$work/path-profiles" "$stage/share/code-view/.install-path-profiles"
cp -- "$work/receipt" "$stage/share/code-view/.install-receipt"
chmod 600 "$stage/share/code-view/.install-path-block" "$stage/share/code-view/.install-path-profiles" "$stage/share/code-view/.install-receipt"
prepare_install_file() {
  local relative=$1 destination=$prefix/$1 source=$stage/$1 temporary index=${#install_targets[@]}
  [[ -f $source ]] || return 0 # Previously owned notices may remain unchanged.
  revalidate_owned_file "$relative"
  check_writable "$destination"
  [[ ! -e $destination || $force == true ]] || fail "already installed; use --force: $destination"
  ensure_directory "$(dirname -- "$destination")"
  if [[ -e $destination ]]; then
    cp -p -- "$destination" "$work/install-original-$index"
    revalidate_owned_file "$relative"
    cmp -s -- "$work/install-original-$index" "$destination" || fail "installation changed while taking snapshot: $destination"
    install_original[index]=true
  else install_original[index]=false; fi
  temporary=$(mktemp "$(dirname -- "$destination")/.code-view-install.XXXXXX")
  temporary_files[${#temporary_files[@]}]=$temporary
  cp -p -- "$source" "$temporary"
  cmp -s -- "$source" "$temporary" || fail 'incomplete installation file write'
  install_targets[index]=$destination
  install_prepared[index]=$temporary
  install_new_hashes[index]=$(hash_file "$source")
}
for relative in ${owned_paths[@]+"${owned_paths[@]}"}; do prepare_install_file "$relative"; done
prepare_install_file share/code-view/.install-receipt
install_committing=true
for ((index=0; index<${#install_targets[@]}; index++)); do
  destination=${install_targets[index]}
  check_file "$destination"
  if [[ ${install_original[index]} == true ]]; then
    [[ -f $destination ]] && cmp -s -- "$work/install-original-$index" "$destination" || fail "installation file changed before commit: $destination"
  else [[ ! -e $destination ]] || fail "installation file appeared before commit: $destination"; fi
  mv -f -- "${install_prepared[index]}" "$destination"
  install_committed=$((install_committed+1))
done
apply_profiles
install_committing=false
printf '%s\n' "Installed $version. Commands: code-view, code-view-uninstall." \
  'Open a new terminal, or add this bin directory to the current PATH:'
printf 'export PATH=%s:"$PATH"\n' "$quoted_bin"
