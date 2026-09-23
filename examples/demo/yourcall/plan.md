# Plan — photo uploads off the request path (brambleway)

## Why

- Uploads resize photos inside the request, so anything over 12 MB times out at 30 s.
- After this change the request only stores the original; thumbnails come later.

## How it works

- The upload endpoint saves the original and returns at once with a grey placeholder.
- A worker picks up a `resize` job and writes the three sizes the app already uses.
- Where it runs is open: the existing mail worker (A) or a new small service (B). I lean A, but it's your pick.

## Rollout

- The code is behind the `async_resize` flag; old photos keep their thumbnails.
- I can switch the flag on this Friday, or hold it until next week's release freeze is over. Which do you want?

## Scope

- Only JPEG and PNG, as today; HEIC uploads still get rejected with the same message.
- While I'm in there I could also emit WebP. Should I, or leave it for a later change?

## Testing

- A 40 MB upload must return in under 1 s, with the thumbnail visible within 10 s.
- The existing upload tests run unchanged against both paths of the flag.

## Risks

- If the worker is down, photos stay grey; the upload itself still succeeds.
