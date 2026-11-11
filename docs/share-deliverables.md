# Share deliverables

`GET /api/status/deliverable?path=share%2Fproject%2Fconcept.html` opens a deliverable behind
AAC sign-in. HEAD has the same checks and headers. Every other method answers 405.
This endpoint is a file reader; it does not modify reports or add UI links.

Clients take `deliverables[].path`, strip only a recognized machine share prefix
(`/mnt/Cursor/`, `/Volumes/Cursor/`, or `\\TRUENAS\Cursor\`), convert Windows separators,
and encode the resulting relative path as one query value. Absolute paths sent to the server
are refused. Do not normalize away traversal segments. Relative assets in concept HTML are
blocked: standalone concepts may use inline styles and data images. Markdown is plain text.

Allowed paths are `share/<file>` and `agent-reports/v1/writers/<writer>/media/<file>`, including
subfolders. The machine share root is fixed by platform, with no request-controlled root.
The request needs the same API guards and browser session as Status. Device tokens, revoked
sessions, signed-out browsers and sign-in-disabled requests cannot read files. A supplied
foreign or null Origin is refused; absent Origin supports direct browser navigation.

## Threat model

Reports and their authors are untrusted. A writer may supply hostile paths, file contents,
symlinks, special files, or swap a parent folder between validation and open. AAC never
assumes that a report author owns or has vetted a linked file. Authenticated owners can
read any allowlisted share file, whether or not a report currently references it. This
is not a per-writer authorization system. Local OS permissions still govern hard links and
who can modify share contents; files may change while streaming, so bytes are not snapshots.

Real paths must stay within the specific allowed root, including a writer's own media root.
The root itself cannot be redirected. Dot segments, dotfiles, dotfolders, empty segments,
NUL/control characters, backslashes, colons, residual percent encodings, absolute paths,
duplicate query values and extra query fields are refused. Directories have no listing.
Regular files are opened read-only with nonblocking and no-follow flags. Linux then checks
the actual open descriptor against the validated path before reading any bytes. This
rejects a parent symlink swap, moved/deleted files, and prevents a FIFO from blocking reads.
Errors expose fixed messages, never private resolved paths.

**Host limitation:** the endpoint currently enables file access only on Linux with working
`/proc/self/fd`. macOS/Windows answer 503 after authentication; missing Linux descriptor
metadata fails closed with 404. Enabling those platforms requires an equivalent native
handle-path check. A second pathname stat would leave a parent-swap race and is not used.

Allowed extensions: html, md, txt, json, png, jpg, jpeg, gif, webp, svg, mp4, webm, pdf.
Types come from this fixed extension table, not writer-supplied MIME values. Every response
has `nosniff`, `no-store`, a no-referrer policy and a CSP sandbox with no origin permission,
scripts, network connections, forms, external resources or nested frames. SVG is an
attachment. HTML can render inline styling/data images, but cannot read AAC cookies or
call its APIs. Files above 100 MiB return 413, even for HEAD/range requests. MP4/WebM support
single byte ranges (206/416); other types ignore Range. Streaming is bounded to the checked
length and closes the descriptor on completion, disconnect or failure.

## Tests

`bun test tests/unit/web-server/status/share-route.test.ts` covers the actual API guard,
browser-only access, revoked sessions, Origin, methods, traversal, roots, symlink escapes,
a deterministic parent-swap race, every content type, HEAD, ranges, empty files and size cap.

`browser-check run web-dashboard/tests/browser/share-route` starts an isolated fixture server
using the real API guard and reader. No dashboard build or provider collectors are needed.
The fixture session is synthetic. A hostile concept tries scripts, cookie access, credentialed
fetches, images and iframes targeting an API counter. Five browser assertions check readability,
script blocking, opaque-origin cookie denial, CSP fetch denial and zero protected API hits.
