# Plan — photo uploads off the request path (brambleway)

## Why

- Uploads resize photos inside the request, so anything over 12 MB times out at 30 s.
- After this change the request only stores the original; thumbnails come later.

## How it works

- The upload endpoint saves the original and returns at once with a grey placeholder.
- A worker picks up a `resize` job and writes the three sizes the app already uses.
- The job runs on the existing mail worker (A); no new service.

## Rollout

- The code is behind the `async_resize` flag; old photos keep their thumbnails.
- The flag goes on once the release freeze ends next week; not this Friday.

## Scope

- Only JPEG and PNG, as today; HEIC uploads still get rejected with the same message.
- No WebP in this change.

## Testing

- A 40 MB upload must return in under 1 s, with the thumbnail visible within 10 s.
- The existing upload tests run unchanged against both paths of the flag.

## Risks

- If the worker is down, photos stay grey; the upload itself still succeeds.
