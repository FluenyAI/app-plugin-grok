import test from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync, readdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { BUNDLE, captureFetch, handshakeBody, makeRepo, useTempConfig } from './helpers.ts'

// Design decision 57's second list, and the drift alarm that keeps it true.
//
// `neverSent` is a claim about the backend and the server can enforce it. This
// list is a claim about a binary on the developer's own laptop, which the server
// can only believe, so the client declares it and the server echoes it attributed
// to this agent and version. That is only worth anything if the declaration keeps
// pace with the code, and a hand-maintained list of what a program reads is
// exactly the kind of thing that stops being true quietly.
//
// So the first test below is not about behaviour. It fails when a module starts
// reading the filesystem without either declaring what it reads or being named as
// infrastructure that touches no developer content.

const here = dirname(fileURLToPath(import.meta.url))
const srcDir = join(here, '..', 'src')

const root = useTempConfig()

const { READS_LOCALLY, readsLocallyDeclaration } = await import('../src/reads.ts')
const { beginSession } = await import('../src/session.ts')
const { writeCredentials } = await import('../src/store.ts')

// Modules that touch the filesystem but never developer content. Each one is
// listed with why, because "it is fine" is what an exemption list degrades into.
const INFRASTRUCTURE: Record<string, string> = {
  'store.ts': 'reads only this client own config, queue and ledger under its config dir',
  'git.ts': 'reads the git remote, which is in the contract and is sent as a hashed repoId',
}

const FS_READ = /\b(readFileSync|readSync|openSync|readdirSync|createReadStream|readFile)\b/

test('a module that reads the filesystem is declared or named as infrastructure', () => {
  const declared = new Set(READS_LOCALLY.map((read) => read.site.replace(/^src\//, '')))
  const undeclared: string[] = []

  for (const name of readdirSync(srcDir)) {
    if (!name.endsWith('.ts')) continue
    const body = readFileSync(join(srcDir, name), 'utf8')
    if (!FS_READ.test(body)) continue
    if (declared.has(name) || name in INFRASTRUCTURE) continue
    undeclared.push(name)
  }

  assert.deepEqual(
    undeclared,
    [],
    `${undeclared.join(', ')} reads the filesystem but is not declared in src/reads.ts ` +
      'and is not named as infrastructure. Add an entry saying what it reads and why, or ' +
      'add it to INFRASTRUCTURE here with a reason. Do not delete this test.',
  )
})

test('every declared read site exists and every entry is answerable', () => {
  assert.ok(READS_LOCALLY.length > 0, 'an empty declaration would render as "nothing is read locally"')
  for (const read of READS_LOCALLY) {
    const name = read.site.replace(/^src\//, '')
    assert.ok(
      readdirSync(srcDir).includes(name),
      `${read.site} is declared but does not exist, so the declaration describes code that is gone`,
    )
    // Rendered verbatim to a developer on a page whose job is being checkable.
    assert.ok(read.what.length > 20, `too terse to be meaningful: ${read.what}`)
    assert.ok(read.why.length > 20, `a read with no stated reason invites the worst reading: ${read.what}`)
    assert.equal(/[–—]/.test(`${read.what} ${read.why}`), false, 'no em or en dashes in developer-facing copy')
  }
})

test('the transcript and the raw payload are both declared, by name', () => {
  // The two the privacy contract actually turns on. Named explicitly so that
  // dropping either one is a failing test rather than a quiet omission.
  const sites = READS_LOCALLY.map((read) => read.site)
  assert.ok(sites.includes('src/extract.ts'), 'the raw tool payload read is not declared')
  assert.ok(sites.includes('src/transcript.ts'), 'the transcript sweep is not declared')
})

test('the declaration is sent at handshake, so the server can echo it attributed', async () => {
  const repoDir = makeRepo(root, 'git@github.com:FluenyAI/app-backend.git')
  writeCredentials({
    apiUrl: 'http://api.test',
    clientId: 'flueny-claude-code',
    accessToken: 'token',
    refreshToken: 'refresh',
    expiresAt: Date.now() + 3_600_000,
  })

  const { calls, restore } = captureFetch(() => ({
    status: 200,
    body: handshakeBody({ bundle: BUNDLE }),
  }))
  try {
    await beginSession({ sessionId: 'session-reads', cwd: repoDir })
  } finally {
    restore()
  }

  const start = calls.find((call) => call.url.includes('/session/start'))
  assert.ok(start, 'no handshake was sent')
  const body = start.body as { readsLocally?: string[]; clientVersion?: string; agent?: string }
  assert.deepEqual(body.readsLocally, readsLocallyDeclaration())
  // Attribution is what makes a client-declared list usable on the privacy page:
  // it is this client saying it, not Flueny asserting it on every client's behalf.
  assert.ok(body.clientVersion, 'the declaration cannot be attributed without a client version')
  assert.ok(body.agent, 'the declaration cannot be attributed without an agent')
})
