#!/usr/bin/env node
// Deterministic packaging fixture; actual installed-Codex acceptance is separate.
import { createInterface } from 'node:readline'
if (process.argv.includes('--version')) {
  process.stdout.write('codex-cli 0.153.4\n')
  process.exit(0)
}
for await (const line of createInterface({ input: process.stdin })) {
  const request = JSON.parse(line)
  if (request.id === undefined) continue
  const result = {
    initialize: { userAgent: 'farhelm-package-fixture', platformFamily: 'unix', platformOs: 'linux' },
    'account/read': { requiresOpenaiAuth: false, account: null },
    'thread/list': { data: [], nextCursor: null },
  }[request.method]
  process.stdout.write(JSON.stringify(result ? { id: request.id, result } : { id: request.id, error: { code: -32601, message: 'Unsupported fixture method' } }) + '\n')
}
