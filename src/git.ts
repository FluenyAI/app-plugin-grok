import { readFileSync, statSync } from 'node:fs'
import { dirname, join, resolve, sep } from 'node:path'

// Where the repository is and what its origin remote is called.
//
// Read from `.git/config` rather than shelled out to `git`, for two reasons. A
// hook of type "command" already pays one process spawn per invocation (eng
// findings 13 and 15), and adding a second one to answer a question a 2KB file
// answers is the wrong trade. And `git remote get-url` inherits the developer's
// environment, which on a machine with credential helpers can prompt.
//
// Nothing here reads a tracked file, a diff or an object. Only `.git/config`.

export interface RepoInfo {
  root: string
  remote: string | null
}

// Walks up from `from` looking for `.git`. `.git` is a directory in a normal
// clone and a file containing `gitdir: <path>` inside a worktree or a submodule,
// and this product is developed in worktrees, so the file form is not exotic.
export function findRepo(from: string): RepoInfo | null {
  let dir = resolve(from)
  for (;;) {
    const dotGit = join(dir, '.git')
    const gitDir = resolveGitDir(dotGit)
    if (gitDir) return { root: dir, remote: readOriginRemote(gitDir) }
    const parent = dirname(dir)
    if (parent === dir) return null
    dir = parent
  }
}

function resolveGitDir(dotGit: string): string | null {
  let stat: ReturnType<typeof statSync>
  try {
    stat = statSync(dotGit)
  } catch {
    return null
  }
  if (stat.isDirectory()) return dotGit
  if (!stat.isFile()) return null
  try {
    const pointer = /^gitdir:\s*(.+)$/m.exec(readFileSync(dotGit, 'utf8'))
    if (!pointer?.[1]) return null
    const target = resolve(dirname(dotGit), pointer[1].trim())
    // A linked worktree's gitdir is `<main>/.git/worktrees/<name>`, whose own
    // config file is usually empty; the remotes live in the main `.git/config`.
    const marker = `${sep}worktrees${sep}`
    const at = target.indexOf(marker)
    return at >= 0 ? target.slice(0, at) : target
  } catch {
    return null
  }
}

// `origin` if it exists, otherwise the first remote in the file. A repo with one
// remote under another name is common enough (`upstream`, a fork) that refusing
// to look would read as "Flueny does not see this repo" with no way to tell why.
export function readOriginRemote(gitDir: string): string | null {
  let text: string
  try {
    text = readFileSync(join(gitDir, 'config'), 'utf8')
  } catch {
    return null
  }
  const remotes = new Map<string, string>()
  let current: string | null = null
  for (const rawLine of text.split('\n')) {
    const line = rawLine.trim()
    const section = /^\[remote\s+"([^"]+)"\]$/.exec(line)
    if (section?.[1]) {
      current = section[1]
      continue
    }
    if (line.startsWith('[')) {
      current = null
      continue
    }
    const url = /^url\s*=\s*(.+)$/.exec(line)
    if (current && url?.[1] && !remotes.has(current)) remotes.set(current, url[1].trim())
  }
  const origin = remotes.get('origin')
  if (origin) return origin
  const first = remotes.values().next()
  return first.done ? null : first.value
}
