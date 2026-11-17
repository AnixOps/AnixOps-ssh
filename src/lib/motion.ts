import type { Transition } from "motion/react";

/* Motion presets from the AnixOps Design motion tokens (guidelines/motion.md):
   120 ms micro, 200 ms toggle, 280 ms overlay, 240 ms page; standard,
   emphasized and exit easing. Only these four durations exist (M7). Only
   opacity and transform are animated (M1), and MotionConfig
   reducedMotion="user" drops transforms when the system asks (M3).

   Selection, page and tab changes, sorting and live data do not animate
   (M8 to M10); presets here are for overlays, disclosure, switches and
   segmented controls. */

export const ease = {
  standard: [0.25, 0.1, 0.25, 1],
  emphasized: [0.2, 0.8, 0.2, 1],
  exit: [0.4, 0, 1, 1],
} as const;

export const duration = { micro: 0.12, toggle: 0.2, overlay: 0.28, page: 0.24 } as const;

/** Overlays enter in `overlay` with the emphasized curve (M6). */
export const enter: Transition = { duration: duration.overlay, ease: ease.emphasized };
/** Leaving is faster than entering: `toggle` with the exit curve (M6). */
export const leave: Transition = { duration: duration.toggle, ease: ease.exit };
/** Segmented control, disclosure, status changes. */
export const toggle: Transition = { duration: duration.toggle, ease: ease.standard };

/** A row the user added or removed fades; neighbours do not slide (M2 table). */
export const appear = {
  initial: { opacity: 0 },
  animate: { opacity: 1, transition: toggle },
  exit: { opacity: 0, transition: { duration: duration.micro, ease: ease.exit } },
};
