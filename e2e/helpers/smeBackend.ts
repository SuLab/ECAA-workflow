/**
 * R14 — Foundation-model-driven SME impersonation backend.
 *
 * Replaces the scripted IVD_FOLLOWUPS bank with a separate
 * Anthropic API caller that plays the SME role in real time. The
 * SME backend reads what the planning assistant just said and
 * responds with a natural, terse SME reply — driving the chat
 * conversation organically rather than via probe-keyed canned
 * answers.
 *
 * Why this exists: the scripted bank's prepared paragraphs hit a
 * scorer ceiling at 13/18 because they don't carry organic
 * conversational structure. A real conversation reveals constraints
 * organically over many turns, which scores cleanly on CONTINUITY,
 * ONE_QUESTION, and CONFIRMATION dimensions.
 *
 * Cost: each per-turn SME reply is one Sonnet 4.6 call (~$0.001-0.005
 * each, much cheaper than the chat-side Opus turns). Total per-run
 * SME-side spend is typically ≤ $0.20.
 *
 * Determinism: temperature 0 + a fixed system prompt + a fixed
 * conversation history → deterministic per-input. Different runs
 * still vary because the chat-side LLM's questions differ.
 *
 * Gating: `ECAA_SPEC_SME_MODE=fm` selects this backend; `scripted`
 * (default) preserves the original IVD_FOLLOWUPS path so CI lanes
 * without an extra API budget keep working.
 */

import Anthropic from '@anthropic-ai/sdk'
import { readFileSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const SME_PROMPT_PATH = join(__dirname, 'sme_role_prompt.txt')

const SME_MODEL = 'claude-sonnet-4-5'
// Sonnet 4.6 is the conversation default in the chat layer too. The
// SME backend doesn't need Opus — impersonation is well within
// Sonnet's range and we don't want the SME burning Opus budget.

const SME_MAX_OUTPUT_TOKENS = 400
// Caps SME replies to enforce terse output. The system prompt asks
// for 1-3 sentences; this is belt-and-suspenders against runaway
// generation.

const SME_TEMPERATURE = 0.0

export interface SmeBackend {
  /** Open the SME's first turn (the chat opener). */
  opener(): Promise<string>
  /**
   * Reply to whatever the assistant just said. Maintains internal
   * conversation history so the SME remembers what they've already
   * said — no need for the spec to thread state.
   */
  reply(assistantContent: string): Promise<string>
  /** Inject the recovery amendment as a natural next-turn message. */
  injectAmendment(amendmentText: string): Promise<void>
  /** Total Anthropic spend on this SME backend (notional in subscription mode). */
  totalCostUsd(): number
}

export function createFmSmeBackend(apiKey: string): SmeBackend {
  const client = new Anthropic({ apiKey })
  const systemPrompt = readFileSync(SME_PROMPT_PATH, 'utf8')
  // Conversation memory: the SME's reply history alternates
  // assistant (SME's prior replies) and user (chat-LLM's prior
  // turns). We invert the role labels because in this sub-conversation
  // the SME is the assistant being scored on the rubric, but in our
  // memory the chat-LLM IS the user we respond to.
  const memory: Array<{ role: 'user' | 'assistant'; content: string }> = []
  let totalCost = 0

  async function callSme(): Promise<string> {
    const resp = await client.messages.create({
      model: SME_MODEL,
      max_tokens: SME_MAX_OUTPUT_TOKENS,
      temperature: SME_TEMPERATURE,
      system: systemPrompt,
      messages: memory,
    })
    const text = resp.content
      .filter((b: { type: string }) => b.type === 'text')
      .map((b: { type: string; text?: string }) => b.text ?? '')
      .join('\n')
      .trim()
    // Approximate notional cost: Sonnet 4.5 input $3/MTok, output $15/MTok.
    const inT = resp.usage.input_tokens ?? 0
    const outT = resp.usage.output_tokens ?? 0
    totalCost += (inT * 3.0 + outT * 15.0) / 1_000_000
    memory.push({ role: 'assistant', content: text })
    return text
  }

  return {
    async opener() {
      // Seed the SME with a soft prompt to start the conversation.
      memory.push({
        role: 'user',
        content:
          '[BEGIN SESSION] The planning assistant just said "Hi! Tell me about ' +
          'the analysis you\'re planning — what kind of data, what question, ' +
          'what you\'ve already done." Open the conversation as the SME would.',
      })
      return callSme()
    },
    async reply(assistantContent: string) {
      memory.push({ role: 'user', content: assistantContent })
      return callSme()
    },
    async injectAmendment(amendmentText: string) {
      // Push the amendment as if the SME themself decided to say it
      // mid-turn. We don't call the LLM here — the spec sends the
      // amendment text directly to the chat. We just record it in
      // memory so the SME remembers having amended.
      memory.push({ role: 'assistant', content: amendmentText })
    },
    totalCostUsd() {
      return totalCost
    },
  }
}
