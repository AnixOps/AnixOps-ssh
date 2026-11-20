import { describe, expect, it } from "vitest";
import { bytesToText, displayText, textToBytes, ZMODEM_START } from "./terminalBytes";
import { engineDecode } from "./engineDecode.testutil";

describe("the byte path", () => {
  it("round-trips every byte value, in any chunking", () => {
    const bytes = new Uint8Array(256 * 3 + 12);
    for (let i = 0; i < bytes.length; i += 1) bytes[i] = i % 256;
    bytes.set(new TextEncoder().encode("主机 \u{1F600}"), 256);
    for (const size of [1, 2, 3, 7, 64, 1000]) {
      const reads = Array.from({ length: Math.ceil(bytes.length / size) }, () => size);
      const text = engineDecode(bytes, reads).join("");
      expect(Array.from(textToBytes(text))).toEqual(Array.from(bytes));
    }
  });

  it("sends bytes back as text the engine turns into the same bytes", () => {
    const bytes = Uint8Array.from([0x00, 0x18, 0x41, 0x80, 0xc3, 0xff, 0x7f]);
    const text = bytesToText(bytes);
    expect(text).toBe("\x00\x18A\u{100080}\u{1000c3}\u{1000ff}\x7f");
    expect(Array.from(textToBytes(text))).toEqual(Array.from(bytes));
  });

  it("handles a large payload", () => {
    const bytes = new Uint8Array(100_000).map((_, i) => (i * 31) & 0xff);
    expect(Array.from(textToBytes(bytesToText(bytes)))).toEqual(Array.from(bytes));
  });

  it("shows escaped bytes as the replacement character in a plain terminal", () => {
    expect(displayText("a\u{1000ff}b\u{100080}")).toBe("a\ufffdb\ufffd");
    // Icon glyphs (Nerd Fonts live in the BMP private-use area) pass through unchanged.
    expect(displayText("\uf780 \uf7ff \ue0b0")).toBe("\uf780 \uf7ff \ue0b0");
    expect(displayText("主机")).toBe("主机");
  });
});

describe("the handshake", () => {
  it("is found in what sz and rz print", () => {
    expect(ZMODEM_START.test("rz\r**\x18B00000000000000\r\x11")).toBe(true);
    expect(ZMODEM_START.test("rz waiting to receive.**\x18B0100000023be50\r\x8a\x11")).toBe(true);
  });

  it("is not found in ordinary output", () => {
    expect(ZMODEM_START.test("** note: B0100 \x18")).toBe(false);
    expect(ZMODEM_START.test("$ ls\r\nfile\r\n")).toBe(false);
  });
});
