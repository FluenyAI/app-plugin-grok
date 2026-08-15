import { createHash } from 'node:crypto'

// Mirror of app-backend/src/integrations/coding/coding-repo-id.ts.
//
// This is a CONTRACT, not an implementation detail. This client hashes the remote
// of the repository it is working in and sends only the hash; the backend compares
// it against the org's allowlist. If the two sides normalize differently, every
// event from a correctly allowlisted repo is dropped and CEO decision 6A
// guarantees nobody sees an error. The steps below are the five written out under
// `## API contract` in app-docs/FEATURES/0028-coding-surface-m1.md, in order.
//
// test/repo-id.test.ts pins the same hash the backend pins, so a drift on either
// side fails a test rather than silently emptying somebody's dashboard.
export const REPO_ID_PREFIX = 'sha256:'

export function normalizeRemote(remote: string): string {
  let s = remote.trim().toLowerCase()
  if (!s) return ''

  s = s.replace(/^git\+/, '')
  // scp-style `git@host:path` has no scheme and its colon is a separator, not a
  // port, so it has to be rewritten before the port strip below.
  const scp = /^([^/@]+@)?([^/:]+):(?!\/\/)(.+)$/.exec(s)
  if (scp && !/^[a-z][a-z0-9+.-]*:\/\//.test(s)) {
    s = `${scp[2]}/${scp[3]}`
  } else {
    s = s.replace(/^[a-z][a-z0-9+.-]*:\/\//, '')
    s = s.replace(/^[^/@]+@/, '')
  }

  s = s.replace(/^([^/]+):\d+/, '$1') // host:port
  // Trailing slashes come off BEFORE the .git suffix, because `.../repo.git/` is
  // a shape git itself accepts and `.git$` would not match through the slash.
  s = s.replace(/\/+$/, '')
  s = s.replace(/\.git$/, '')
  s = s.replace(/\/+$/, '')
  s = s.replace(/\/{2,}/g, '/')
  return s
}

export function repoIdFor(remote: string): string {
  const normalized = normalizeRemote(remote)
  if (!normalized) return ''
  return REPO_ID_PREFIX + createHash('sha256').update(normalized).digest('hex')
}
