/* Test helper: the same decoding the engine does to terminal bytes. */
/** What the engine does to terminal bytes (crates/easyssh-engine/src/utf8.rs): valid
 *  UTF-8 becomes text, any other byte becomes U+F700 + byte, a character split across
 *  reads is held back. `reads` are the sizes of the chunks the bytes arrive in. */
export function engineDecode(bytes: Uint8Array, reads: number[]): string[] {
  const out: string[] = [];
  let pending: number[] = [];
  let at = 0;
  const need = (lead: number) => (lead >= 0xf0 && lead <= 0xf4 ? 4 : lead >= 0xe0 ? 3 : lead >= 0xc2 && lead < 0xe0 ? 2 : 1);
  const take = (chunk: Uint8Array, last: boolean) => {
    pending.push(...chunk);
    let text = "";
    let i = 0;
    while (i < pending.length) {
      const lead = pending[i];
      if (lead < 0x80) {
        text += String.fromCharCode(lead);
        i += 1;
        continue;
      }
      const length = need(lead);
      if (length === 1 || lead > 0xf4) {
        text += String.fromCharCode(0xf700 + lead);
        i += 1;
        continue;
      }
      if (i + length > pending.length) {
        if (last) {
          for (; i < pending.length; i += 1) text += String.fromCharCode(0xf700 + pending[i]);
        }
        break;
      }
      const slice = Uint8Array.from(pending.slice(i, i + length));
      try {
        text += new TextDecoder("utf-8", { fatal: true }).decode(slice);
        i += length;
      } catch {
        text += String.fromCharCode(0xf700 + lead);
        i += 1;
      }
    }
    pending = pending.slice(i);
    if (text) out.push(text);
  };
  for (const size of reads) {
    take(bytes.subarray(at, at + size), false);
    at += size;
  }
  take(new Uint8Array(), true);
  return out;
}
