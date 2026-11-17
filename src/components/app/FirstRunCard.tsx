import { Button } from "@/components/ui/button";

export interface FirstRunStrings {
  title: string;
  body: string;
  allow: string;
  deny: string;
}

/** A small card in the corner, not a dialog: the app stays usable around it.
    Two buttons of the same look and size; neither is focused or marked. */
export function FirstRunCard({ strings, onAnswer }: { strings: FirstRunStrings; onAnswer: (allow: boolean) => void }) {
  return (
    <section
      role="region"
      aria-labelledby="first-run-title"
      className="fixed right-4 bottom-4 z-toast w-80 max-w-[calc(100vw-2rem)] rounded-md bg-bg-elevated p-4 shadow-2 ring-1 ring-separator"
    >
      <h2 id="first-run-title" className="text-callout font-medium text-label-1">
        {strings.title}
      </h2>
      <p className="mt-1 text-callout text-label-2">{strings.body}</p>
      <div className="mt-3 grid grid-cols-2 gap-2">
        <Button variant="secondary" size="sm" onClick={() => onAnswer(true)}>
          {strings.allow}
        </Button>
        <Button variant="secondary" size="sm" onClick={() => onAnswer(false)}>
          {strings.deny}
        </Button>
      </div>
    </section>
  );
}
