# Separate caches for separate skill sources

On Windows, catalog Git commands enable `core.longpaths` for that command only.
When the cached `.git/config` path reaches 240 UTF-16 units, Git works in a
private system temporary directory: some Git startup paths reject a long
repository path before reading that option. A successful shallow clone is
copied to a prepared directory beside the cache, then replaces it with rollback
on a failed rename. Refresh uses the same exact configured source and prepares
a fresh clone. A warm search continues to reuse the cache without Git or HTTP.

Preparation uses the existing bounded file-tree reader: 2,000 entries, 64 MiB
in total and 8 MiB per file. These are copy limits, not a network-download or
whole-operation deadline. Failed clone/copy preparation leaves the old cache
intact; unexpected non-cache destinations are refused. If replacement and
restoration both fail, the error identifies the retained previous directory.
Git's temporary working path must remain within its supported startup limits.
No global/system Git configuration or installed skill is changed by discovery.
The native Windows fixture covers 14 deep-home conditions, including refused
refresh, copy-ceiling failure and unknown-destination preservation. Another
10 conditions cover long pack paths while config remains below the bootstrap
threshold, and the usual 10 cover a normal home. Source/config path lengths are
checked in UTF-16 units. Two publication tests separately require rollback of
an existing cache and absence of a ready cache after a failed first publication.
See the [Git for Windows FAQ](https://github.com/git-for-windows/git-for-windows.github.io/blob/main/content/faq.md).

Remote skill discovery used a readable directory made by replacing URL
punctuation with hyphens. Different addresses such as `/a/b` and `/a-b` therefore
shared a cache. A controlled published 0.21.9 CLI search returned the first
repository's skill under both source URLs without contacting the second one.
Refreshing repeated the first repository's fetch and still attributed its skill
to the second source.

A remote source now receives a short readable repository label followed by the
full SHA256 of its exact configured address. The label excludes userinfo, query
and fragment, admits only ASCII letters/digits/hyphens, and is bounded to
24 characters. Dots are replaced so labels such as `CON.foo` cannot become a
reserved Windows device path. The complete directory component is at most 89 bytes. The digest
keeps punctuation, case, escaping, query values, trailing slash and `.git`
distinct: arbitrary configured HTTP servers can give those addresses different
meanings. Repeated use of the same address selects the same directory.

Legacy directories remain in place. A subsequent explicit search fetches into
the new address-specific directory; it never migrates, deletes or treats a
lossy legacy name as proof of source identity. The digest identifies a
configuration string; transport authentication and content trust continue
through the existing Git and skill-install controls.

Remote origin selection also compares the exact configured address. Previously
it removed `.git` and a trailing slash, so explicit `--source ADDRESS` could
install a same-named skill from a different endpoint, and updates could continue
using that substituted origin. A cache-only candidate reproduced both failures
even after all eight search/cache conditions passed. Installation and saved
update origins now retain the same distinctions as the cache key. Equivalent
local directory paths still canonicalize to the same identity. Existing
installed content and local-edit protection remain in place. If an origin's
configured spelling changes, use `rook skills install NAME --source ADDRESS`
for an intentional source migration instead of assuming the addresses alias.

Reading pinned Hermes `22063fc099ec9ad8357d7f8f6e6248a0400680d3`,
`tools/skills_hub_github.py::_tap_cache_key`, prompted this identity audit.
That reference includes repository/path/bucket in its key but also replaces
punctuation; it is not claimed collision-proof and its mapping is not copied.
Rook independently uses its existing SHA256 dependency. No dependency, schema,
configuration setting or MSRV changes.

Unit tests cover the colliding addresses, exact-address distinctions in both
cache and origin selection, equivalent local paths, stable reuse, long
ASCII/Unicode names and directory boundaries. The
[actual CLI fixture](../xtask/probes/catalog-source-identity/README.md) serves two
owned repositories over smart HTTP, seeds a legacy cache and verifies cold,
warm and refresh behavior plus original-cache/workspace/binary preservation.
It additionally installs a same-named skill from the explicitly selected `.git`
endpoint, advances that repository and verifies updated manifest/resource bytes
come from the recorded exact origin.
These are source-identity checks, independent of live-model task quality.
