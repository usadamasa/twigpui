# Media: attached images and author avatars

## Attached images

A post's attached images render as thumbnails under its text. Videos and
animated GIFs show their still with a "Video" / "GIF" badge — neither plays
here; that is deliberately out of scope. Clicking a thumbnail opens the full
image in the browser (#70), since this app has no lightbox.

`attachments.media_keys` and the `media.fields` join ride along in the
timeline request already being made, so **no extra request and no extra
cost** — only a larger response. The images themselves come from
`pbs.twimg.com` and share the download-once cache layer with avatars (see
below), under `$XDG_CACHE_HOME/twigpui/media/`.

**Thumbnail cells are a fixed height**, not sized from each image's own
`width`/`height`. A row whose height depends on which images have finished
downloading reflows under the reader as they land, which is worse than
showing frames waiting to be filled. One image renders across a single
column, two or more in two columns — three across would each be too narrow
to read, and X's own maximum of four is two rows of two.

**Alt text is shown, not hidden behind a hover.** This app has no
screen-reader path of its own, so alt text a sighted reader can actually see
is more use than alt text nobody ever reaches.

## Author avatars

Each row shows the author's profile image, 44pt and circular, to the left of
the byline. `user.fields=profile_image_url` rides along in the timeline
request that was already being made, so the URL costs nothing extra; the
image itself is fetched from `pbs.twimg.com`, which is **not** the X API —
no quota, no credits, nothing for the usage tracking to count.

**Downloaded once, off the UI thread.** Images are cached under
`$XDG_CACHE_HOME/twigpui/avatars/`, keyed by a hash of the URL (X reuses the
same basename across accounts, so anything shorter would collide). A
timeline where one author posts ten times downloads one image. Fetching runs
on the background executor one URL at a time, and each avatar appears as it
lands rather than the timeline waiting for the slowest.

**Until then — or if it fails — a placeholder** the exact same size holds
the space, carrying the author's initial. A row never reflows when an image
arrives, and an author whose name never expanded gets the bare circle rather
than an invented character. A failed download is simply left absent and
retried on the next reload; there is nothing useful to tell the user about
an avatar that didn't load.

**Size.** X's `profile_image_url` ends in `_normal` (48×48), which blurs on
a Retina display, so twigpui asks for the `_400x400` variant instead. That
suffix convention is X's own and not promised by the API, so a URL that
doesn't match is used unchanged, and a rewritten URL that fails falls back
to the original rather than leaving the row without a face.

Losing the avatar cache costs one re-download per author and nothing else —
it is cache, not state.

