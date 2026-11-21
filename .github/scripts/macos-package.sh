#!/usr/bin/env bash
set -euo pipefail

# The Nix output's bin entries are store-bound launchers. Use the real pair.
binaries=$1
terminfo=$2
openssl_source=$3
output=$4
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
root=$work/root
runtime=$root/lib/bkpr
mkdir -p "$runtime/bin" "$runtime/lib" "$runtime/share" "$root/bin" "$root/share"
for program in bkpr bkprd; do
  install -m755 "$binaries/bin/$program" "$runtime/bin/$program"
  ln -s "../lib/bkpr/bin/$program" "$root/bin/$program"
done
ditto "$terminfo/share/terminfo" "$runtime/share/terminfo"
ditto "$binaries/share" "$root/share"
ditto "$terminfo/share/licenses" "$root/share/licenses"
ditto "$terminfo/share/bkpr" "$root/share/bkpr"

# OpenSSL is the only non-system dynamic dependency we expect. Reject other
# load paths rather than shipping a package that works only on the Nix runner.
loads() {
  otool -arch arm64 -l "$1" | awk '
    $1 == "cmd" { field = ($2 ~ /^LC_.*DYLIB$/ && $2 != "LC_ID_DYLIB") ? "name" : ($2 == "LC_RPATH" ? "path" : "") }
    field != "" && $1 == field { print field, $2; field = "" }
  '
}
relocate() {
  local file=$1 relative=$2 load kind path name lib
  load=$(loads "$file")
  while read -r kind path; do
    case "$kind:$path" in
      :) ;;
      path:*) install_name_tool -delete_rpath "$path" "$file" ;;
      name:/usr/lib/*|name:/System/Library/*) ;;
      name:/nix/store/*/lib/libssl.3.dylib|name:/nix/store/*/lib/libcrypto.3.dylib)
        name=${path##*/}
        lib=$runtime/lib/$name
        if [[ ! -e "$lib" ]]; then
          install -m755 "$path" "$lib"
          install_name_tool -id "@loader_path/$name" "$lib"
          relocate "$lib" @loader_path
        fi
        install_name_tool -change "$path" "$relative/$name" "$file"
        ;;
      *) echo "Unexpected Mach-O load command in $file: $kind $path" >&2; return 1 ;;
    esac
  done <<< "$load"
}
for program in bkpr bkprd; do
  relocate "$runtime/bin/$program" @loader_path/../lib
done
if compgen -G "$runtime/lib/*.dylib" > /dev/null; then
  mkdir -p "$root/share/licenses/openssl" "$work/openssl"
  tar -xf "$openssl_source" -C "$work/openssl" --strip-components 1 --include '*/LICENSE.txt'
  install -m644 "$work/openssl/LICENSE.txt" "$root/share/licenses/openssl/LICENSE.txt"
fi

if [[ ${MACOS_KEYCHAIN:-} ]]; then
  : "${MACOS_APPLICATION_IDENTITY:?Missing Developer ID Application identity}"
fi
while IFS= read -r -d '' file; do
  if [[ ${MACOS_KEYCHAIN:-} ]]; then
    codesign --force --options runtime --timestamp --keychain "$MACOS_KEYCHAIN" \
      --sign "$MACOS_APPLICATION_IDENTITY" "$file"
  else
    codesign --force --sign - "$file"
  fi
  codesign --verify --strict "$file"
done < <(find "$runtime/bin" "$runtime/lib" -type f -print0)

version=$("$runtime/bin/bkpr" --version)
version=${version#bkpr }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
pkgbuild --root "$root" --install-location /usr/local --identifier app.bkpr.cli \
  --version "$version" --ownership recommended "$work/payload.pkg"
cat > "$work/requirements.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict><key>arch</key><array><string>arm64</string></array></dict>
</plist>
PLIST
productbuild --product "$work/requirements.plist" --package "$work/payload.pkg" "$output"
