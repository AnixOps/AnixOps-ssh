import { describe, expect, it } from "vitest";
import { checkWorkflow } from "./check-workflows.mjs";

describe("checkWorkflow", () => {
  it("accepts npm ci and a locked cargo check", () => {
    const yml = "steps:\n  - run: npm ci\n  - run: cargo metadata --locked --format-version 1\n  - run: cargo test --workspace\n";
    expect(checkWorkflow("ok.yml", yml)).toEqual([]);
  });

  it("rejects npm install, npm i and npm update", () => {
    for (const cmd of ["npm install", "npm i", "npm update", "mkdir -p logs && npm install > log 2>&1"]) {
      expect(checkWorkflow("w.yml", `  - run: ${cmd}\n`)).toHaveLength(1);
    }
  });

  it("ignores comments", () => {
    expect(checkWorkflow("w.yml", "  # do not use npm install here\n  - run: npm ci\n")).toEqual([]);
  });

  it("requires --locked once when the workflow builds Rust", () => {
    expect(checkWorkflow("w.yml", "  - run: cargo build -p x\n")).toHaveLength(1);
    expect(checkWorkflow("w.yml", "  - run: npx tauri build\n")).toHaveLength(1);
    expect(checkWorkflow("w.yml", "  - run: cargo build --locked -p x\n")).toEqual([]);
  });
});
