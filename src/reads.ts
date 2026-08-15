// What this client reads on the machine, derives from, and discards.
//
// Design decision 57 split one promise into two. `neverSent` is a claim about
// the backend: it describes what the server will not accept or store, the server
// can enforce it, and the server owns it. This is the other claim, and it is a
// claim about a binary on someone else's laptop. The server cannot enforce it and
// can only believe it, which is exactly why the client declares it at handshake
// rather than the server asserting it on the client's behalf. A confident false
// statement about the developer's own machine, on the one page whose entire value
// is being checkable, is the worst available outcome.
//
// It is attributed by the `agent` and `clientVersion` already on
// `SessionStartRequest`, so `/coding/privacy` can say which client made the claim
// rather than stating it as a property of Flueny.
//
// THE RULE THIS FILE EXISTS TO ENFORCE: a new local read gets an entry here in
// the same commit. `test/reads.test.ts` fails when a module starts reading the
// filesystem without either declaring what it reads or being listed as
// infrastructure that touches no developer content. That is the same principle as
// `fieldsSent` in the receipt, which is generated from the serialized event rather
// than written by hand, because a hand-written description of behaviour drifts
// from the behaviour and the drift is silent.

export interface LocalRead {
  /** Developer-facing, rendered verbatim on `/coding/privacy`. */
  what: string
  /** The module that performs the read. Checked by the drift test. */
  site: string
  /** Why it is read at all, so the page can answer "and why do you need that". */
  why: string
}

export const READS_LOCALLY: LocalRead[] = [
  {
    what: 'Raw tool inputs and outputs, including file contents, diffs and command output',
    site: 'src/extract.ts',
    why: 'To derive which class of path was touched and whether tests ran. Nothing read here is transmitted.',
  },
  {
    what: 'The session transcript, tool-use decision records only',
    site: 'src/transcript.ts',
    why: 'Claude Code does not fire a hook when you decline an edit, so declines exist nowhere else. Prompt text and assistant replies are never decoded.',
  },
]

/** The wire form. Order is stable so a diff between two client versions is readable. */
export function readsLocallyDeclaration(): string[] {
  return READS_LOCALLY.map((read) => read.what)
}
