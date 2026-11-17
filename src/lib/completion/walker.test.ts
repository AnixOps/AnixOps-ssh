import { describe, expect, it } from "vitest";
import { analyze } from "./tokenizer";
import type { CommandSpec } from "./types";
import { loadSpec } from "./specs";
import { walk, type WalkResult } from "./walker";

/** Walks the real spec for the first word of `line`. */
async function walkLine(line: string): Promise<WalkResult> {
  const { words, current } = analyze(line);
  const spec = (await loadSpec(words[0])) as CommandSpec;
  expect(spec, `a spec for ${words[0]}`).not.toBeNull();
  return walk(spec, words.slice(1), current.text);
}

const labels = (result: WalkResult) => result.candidates.map((candidate) => candidate.label);

describe("walk with the shipped specs", () => {
  it("offers subcommands for git", async () => {
    const result = await walkLine("git ");
    expect(labels(result)).toEqual(expect.arrayContaining(["commit", "checkout", "push"]));
    expect(result.candidates[0].kind).toBe("subcommand");
  });

  it("git commit -m takes a message and then offers options again", async () => {
    const afterFlag = await walkLine("git commit -m ");
    expect(afterFlag.candidates).toEqual([]);
    expect(afterFlag.templates).toEqual([]);
    const next = await walkLine('git commit -a -m "fix: a bug" --');
    expect(labels(next)).toContain("--amend");
    expect(labels(next)).not.toContain("-a");
    expect(labels(next)).toContain("--message");
  });

  it("does not offer an option twice unless it is repeatable", async () => {
    const result = await walkLine("git commit --amend --");
    expect(labels(result)).not.toContain("--amend");
    const docker = await walkLine("docker run -e A=1 --");
    expect(labels(docker)).toContain("--env");
  });

  it("docker run -it --rm image: the image comes after the flags, the command after the image", async () => {
    const image = await walkLine("docker run -it --rm ");
    expect(image.templates).toContain("history");
    expect(image.partial).toBe("");
    const afterImage = await walkLine("docker run -it --rm ubuntu:24.04 ");
    expect(afterImage.wrap).toBeUndefined();
    expect(afterImage.candidates).toEqual([]);
    // Options after the image belong to the container's command.
    const flagsAfter = await walkLine("docker run -it --rm ubuntu:24.04 ls -");
    expect(labels(flagsAfter)).toEqual([]);
  });

  it("docker run --name takes a value", async () => {
    const result = await walkLine("docker run --name ");
    expect(result.templates).toEqual([]);
    expect(result.candidates).toEqual([]);
  });

  it("kubectl get pods -n waits for a namespace", async () => {
    const result = await walkLine("kubectl get pods -n ");
    expect(result.templates).toContain("history");
    const options = await walkLine("kubectl get pods -");
    expect(labels(options)).toEqual(expect.arrayContaining(["-n", "--namespace", "-o", "--output"]));
    const types = await walkLine("kubectl get ");
    expect(labels(types)).toEqual(expect.arrayContaining(["pods", "deployments"]));
  });

  it("kubectl: a global option before the verb is read and the verb still found", async () => {
    const result = await walkLine("kubectl -n prod get pods ");
    expect(result.path).toEqual(["get"]);
  });

  it("tar -xzvf waits for the archive path", async () => {
    const result = await walkLine("tar -xzvf ");
    expect(result.templates).toEqual(["filepaths"]);
  });

  it("tar clusters short flags and keeps going after the file", async () => {
    const afterFile = await walkLine("tar -xzvf a.tgz -C ");
    expect(afterFile.templates).toEqual(["folders"]);
    const hint = await walkLine("tar -xz");
    expect(labels(hint)).toEqual(expect.arrayContaining(["-xzv", "-xzf"]));
  });

  it("hides options that exclude one already used", async () => {
    const result = await walkLine("tar -x -");
    expect(labels(result)).not.toContain("-c");
    expect(labels(result)).not.toContain("--create");
    expect(labels(result)).toContain("-f");
  });

  it("sudo hands over to the wrapped command", async () => {
    const result = await walkLine("sudo systemctl restart ");
    expect(result.wrap).toEqual(["systemctl", "restart"]);
    const first = await walkLine("sudo ");
    expect(first.templates).toEqual(["command"]);
    const withOption = await walkLine("sudo -u deploy ");
    expect(withOption.templates).toEqual(["command"]);
  });

  it("find does not cluster single dash long options", async () => {
    const result = await walkLine("find . -name ");
    expect(result.candidates).toEqual([]);
    const options = await walkLine("find . -ty");
    expect(labels(options)).toContain("-type");
  });

  it("-- ends the options", async () => {
    const result = await walkLine("rm -- -");
    expect(result.candidates).toEqual([]);
    expect(result.templates).toEqual(["filepaths"]);
  });

  it("completes the value of --opt=value", async () => {
    const result = await walkLine("ls --color=al");
    expect(result.valuePrefix).toBe("--color=");
    expect(result.partial).toBe("al");
    expect(labels(result)).toEqual(expect.arrayContaining(["always"]));
  });

  it("requiresEquals options insert the equals sign", async () => {
    const result = await walkLine("ls --col");
    const option = result.candidates.find((candidate) => candidate.label === "--color");
    expect(option?.insert).toBe("--color=");
    expect(option?.keepOpen).toBe(true);
  });

  it("walks nested subcommands", async () => {
    expect((await walkLine("docker compose up -")).path).toEqual(["compose", "up"]);
    expect(labels(await walkLine("docker compose "))).toEqual(expect.arrayContaining(["up", "down", "logs"]));
    expect(labels(await walkLine("docker compose -f a.yml up -"))).toContain("--detach");
    expect(labels(await walkLine("git stash "))).toEqual(expect.arrayContaining(["pop", "list"]));
  });

  it("takes aliases of subcommands", async () => {
    expect((await walkLine("npm i -")).path).toEqual(["install"]);
    expect((await walkLine("ip a show ")).path).toEqual(["address", "show"]);
  });
});

describe("walk with a small spec", () => {
  const spec: CommandSpec = {
    name: "demo",
    description: "demo",
    parser: { flagClustering: false },
    options: [
      { name: "-v", description: "verbose", repeatable: true },
      { name: "-vv", description: "very verbose" },
      { name: "--mode", description: "mode", args: [{ suggestions: ["fast", { name: "slow", description: "Slower" }] }] },
    ],
    args: [{ name: "a" }, { name: "rest", variadic: true, template: "filepaths" }],
  };

  it("reads a clustered flag as one word when clustering is off", () => {
    const result = walk(spec, ["-vv"], "-");
    const names = result.candidates.map((candidate) => candidate.label);
    expect(names).toContain("-v");
    expect(names).not.toContain("-vv");
    // No cluster hints either.
    expect(names.filter((name) => /^-[a-z]{2,}$/.test(name))).toEqual([]);
  });

  it("repeats the last variadic argument", () => {
    expect(walk(spec, ["one", "two", "three"], "").templates).toEqual(["filepaths"]);
    expect(walk(spec, [], "").templates).toEqual([]);
  });

  it("offers static suggestions with descriptions", () => {
    const result = walk(spec, ["--mode"], "");
    expect(result.candidates).toEqual([
      { label: "fast", insert: "fast", kind: "value" },
      { label: "slow", insert: "slow", kind: "value", description: "Slower" },
    ]);
  });
});
