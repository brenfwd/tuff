# Shared vanilla-server setup for scripts/vanilla-compat.sh and
# scripts/vanilla-parity-run.sh. Source after setting $root.
# vanilla_locate: sets version, jar and java from the tuff-vanilla-data build
#   cache (VANILLA_CACHE_DIR, VANILLA_JAVA override), exits if missing.
# vanilla_prepare DIR [EXTRA_PROPERTY_LINES...]: writes eula.txt and an
#   offline, port-0 server.properties into DIR.
. "$root/scripts/lib/timeout.sh"

vanilla_locate() {
    version=26.3
    local cache=${VANILLA_CACHE_DIR:-$root/target/vanilla}
    jar="$cache/$version/server.jar"
    java=${VANILLA_JAVA:-$(find "$cache/java" -path '*/bin/java' -type f 2>/dev/null | head -n 1)}
    [[ -f "$jar" ]] || { echo "missing $jar; run cargo build first" >&2; exit 1; }
    [[ -x "$java" ]] || { echo "no JVM found; set VANILLA_JAVA" >&2; exit 1; }
}

vanilla_prepare() {
    local dir=$1; shift
    echo "eula=true" > "$dir/eula.txt"
    {
        printf '%s\n' level-name=world online-mode=false server-port=0 enable-status=false management-server-enabled=false
        [[ $# -gt 0 ]] && printf '%s\n' "$@"
    } > "$dir/server.properties"
}
