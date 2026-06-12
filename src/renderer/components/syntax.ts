/**
 * Syntax parsing + execution for reproducible analyses. The grammar is
 * deliberately tiny — one command per line:
 *
 *   RUN   <procedure> <jsonParams>
 *   CHART <kind>      <jsonParams>
 *
 * Blank lines and lines starting with `*`, `#`, or `//` are comments. Running
 * the syntax replays each command and appends its result to the Output viewer.
 */
import { outputStore } from '../stores/outputStore';
import type { Analysis, ChartData } from '../types/analysis';

export interface SyntaxResult {
  ran: number;
  errors: string[];
}

/** Format a single replayable command line. */
export function syntaxLine(kind: 'RUN' | 'CHART', name: string, params: Record<string, unknown>): string {
  return `${kind} ${name} ${JSON.stringify(params)}`;
}

/** Parse and execute every command in `text`, appending results to the output. */
export async function runSyntax(text: string): Promise<SyntaxResult> {
  const result: SyntaxResult = { ran: 0, errors: [] };
  const lines = text.split('\n');

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!.trim();
    if (line === '' || line.startsWith('*') || line.startsWith('#') || line.startsWith('//')) {
      continue;
    }
    const parsed = parseLine(line);
    if ('error' in parsed) {
      result.errors.push(`Line ${i + 1}: ${parsed.error}`);
      continue;
    }
    try {
      if (parsed.kind === 'RUN') {
        const a = await window.electron.analysis.run(parsed.name, parsed.params);
        outputStore.appendAnalysis(a as Analysis);
      } else {
        const c = await window.electron.analysis.chart(parsed.name, parsed.params);
        outputStore.appendChart(c as ChartData);
      }
      result.ran++;
    } catch (err) {
      result.errors.push(`Line ${i + 1}: ${err}`);
    }
  }
  return result;
}

type Parsed =
  | { kind: 'RUN' | 'CHART'; name: string; params: Record<string, unknown> }
  | { error: string };

function parseLine(line: string): Parsed {
  const brace = line.indexOf('{');
  if (brace < 0) return { error: 'missing JSON parameters (expected `{ … }`)' };
  const head = line.slice(0, brace).trim().split(/\s+/);
  if (head.length < 2) return { error: 'expected `RUN <procedure> {…}` or `CHART <kind> {…}`' };
  const kw = head[0]!.toUpperCase();
  if (kw !== 'RUN' && kw !== 'CHART') return { error: `unknown command '${head[0]}'` };
  let params: Record<string, unknown>;
  try {
    params = JSON.parse(line.slice(brace));
  } catch (err) {
    return { error: `invalid JSON (${err})` };
  }
  return { kind: kw, name: head[1]!, params };
}
