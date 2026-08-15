// The path classifier. Eng finding 9: one classifier feeds extraction here today
// and Cedar at M3, so the rules ship in the policy bundle rather than being
// compiled into the client. What does NOT ship is the sensitive-path list the
// scorers use, per eng finding 12, so nothing here can be read back as "which
// paths Flueny grades you on".
//
// The output is a single short label like "tests" or "auth". The path itself is
// never transmitted, and this is the only thing derived from it.

// Backend order (coding-bundle.service.ts) is significant: first match wins, and
// the broad language buckets are last. Object key order is the contract, so the
// bundle must be iterated in insertion order and never sorted.
export function classifyPath(classifier: Record<string, string[]>, relPath: string): string | null {
  const path = relPath.replace(/\\/g, '/').replace(/^\.\//, '').replace(/^\/+/, '')
  if (!path) return null
  for (const [pathClass, patterns] of Object.entries(classifier)) {
    for (const pattern of patterns) {
      if (globToRegExp(pattern).test(path)) return pathClass
    }
  }
  return null
}

const cache = new Map<string, RegExp>()

// A deliberately small glob subset: `**`, `*` and `?`, which is everything the
// M1 bundle uses. A general glob library would be a runtime dependency in a
// process that spawns once per tool call, and the patterns are server-authored.
export function globToRegExp(pattern: string): RegExp {
  const cached = cache.get(pattern)
  if (cached) return cached

  let out = ''
  for (let i = 0; i < pattern.length; i++) {
    const ch = pattern[i]
    if (ch === '*') {
      const doubled = pattern[i + 1] === '*'
      if (doubled) {
        // `**/` also has to match zero directories, so `**/*.md` matches
        // `README.md` at the root and not only `docs/README.md`.
        if (pattern[i + 2] === '/') {
          out += '(?:.*/)?'
          i += 2
        } else {
          out += '.*'
          i += 1
        }
      } else {
        out += '[^/]*'
      }
      continue
    }
    if (ch === '?') {
      out += '[^/]'
      continue
    }
    out += ch!.replace(/[.+^${}()|[\]\\]/g, '\\$&')
  }
  const re = new RegExp(`^${out}$`)
  cache.set(pattern, re)
  return re
}
