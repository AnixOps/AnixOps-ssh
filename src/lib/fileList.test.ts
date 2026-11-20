import { describe, expect, it } from "vitest";
import type { RemoteFile } from "@/lib/backend";
import { listingTime, nextSort, sortEntries, visibleEntries } from "@/lib/fileList";

const entry = (name: string, kind: RemoteFile["kind"], size: number, modified: string): RemoteFile => ({
  name,
  kind,
  size,
  modified,
  permissions: kind === "directory" ? "drwxr-xr-x" : "-rw-r--r--",
});

const now = new Date(2026, 10, 20, 12, 0);
const entries = [
  entry("file10.txt", "file", 50, "Nov 19 08:00"),
  entry("file2.txt", "file", 5000, "Jan  3  2025"),
  entry("src", "directory", 4096, "Nov 20 09:00"),
  entry(".env", "file", 10, "Dec 30 10:00"),
  entry("Docs", "directory", 4096, "Oct  1  2024"),
];
const names = (list: RemoteFile[]) => list.map((item) => item.name);

describe("listingTime", () => {
  it("reads recent and older ls dates", () => {
    expect(listingTime("Nov 19 08:00", now)).toBe(new Date(2026, 10, 19, 8, 0).getTime());
    expect(listingTime("Jan  3  2025", now)).toBe(new Date(2025, 0, 3).getTime());
  });

  it("puts a date without a year that lies ahead in last year", () => {
    expect(listingTime("Dec 30 10:00", now)).toBe(new Date(2025, 11, 30, 10, 0).getTime());
  });

  it("gives null for anything else", () => {
    expect(listingTime("", now)).toBeNull();
    expect(listingTime("2026-11-20", now)).toBeNull();
  });
});

describe("sortEntries", () => {
  it("puts folders first and orders names naturally", () => {
    expect(names(sortEntries(entries, { key: "name", descending: false }, now))).toEqual([
      "Docs",
      "src",
      ".env",
      "file2.txt",
      "file10.txt",
    ]);
  });

  it("sorts files by size, folders by name", () => {
    expect(names(sortEntries(entries, { key: "size", descending: true }, now))).toEqual([
      "Docs",
      "src",
      "file2.txt",
      "file10.txt",
      ".env",
    ]);
  });

  it("sorts by modified time, newest first", () => {
    expect(names(sortEntries(entries, { key: "modified", descending: true }, now))).toEqual([
      "src",
      "Docs",
      "file10.txt",
      ".env",
      "file2.txt",
    ]);
  });
});

describe("nextSort", () => {
  it("reverses the same column and starts size and time descending", () => {
    expect(nextSort({ key: "name", descending: false }, "name")).toEqual({ key: "name", descending: true });
    expect(nextSort({ key: "name", descending: false }, "size")).toEqual({ key: "size", descending: true });
    expect(nextSort({ key: "size", descending: true }, "name")).toEqual({ key: "name", descending: false });
  });
});

describe("visibleEntries", () => {
  it("drops dot files when hidden ones are not shown", () => {
    expect(names(visibleEntries(entries, false))).not.toContain(".env");
    expect(names(visibleEntries(entries, true))).toContain(".env");
  });
});
