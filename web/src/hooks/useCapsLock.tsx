import { useCallback, useEffect, useState } from "react";

const EVENT_KEY_DOWN = "keydown";
const EVENT_KEY_UP = "keyup";

/* Hook to verify state of Caps Lock */
export function useCapsLock(): boolean {
  /* State for keeping track of whether caps lock is on */
  const [isCaplocksActive, setIsCapLocksActive] = useState<boolean>(false);

  /* Function to see if caps lock was activated - returns a memorized callback */
  const wasCapsLockActivated = useCallback(
    (event: KeyboardEvent) => {
      if (
        event.getModifierState &&
        event.getModifierState("CapsLock") &&
        isCaplocksActive === false
      ) {
        setIsCapLocksActive(true);
      }
    },
    [isCaplocksActive],
  );

  /* Function to see if caps lock was deactivated - returns a memorized callback */
  const wasCapsLockDeactivated = useCallback(
    (event: KeyboardEvent) => {
      if (
        event.getModifierState &&
        !event.getModifierState("CapsLock") &&
        isCaplocksActive
      ) {
        setIsCapLocksActive(false);
      }
    },
    [isCaplocksActive],
  );

  useEffect(() => {
    /* Add keydown event listener */
    document.addEventListener(EVENT_KEY_DOWN, wasCapsLockActivated);
    /* Remove keydown event listener on cleanup */
    return () => {
      document.removeEventListener(EVENT_KEY_DOWN, wasCapsLockActivated);
    };
  }, [wasCapsLockActivated]);

  useEffect(() => {
    /* Add keyup event listener */
    document.addEventListener(EVENT_KEY_UP, wasCapsLockDeactivated);
    return () => {
      /* Remove keyup event listener on cleanup */
      document.removeEventListener(EVENT_KEY_UP, wasCapsLockDeactivated);
    };
  }, [wasCapsLockDeactivated]);

  return isCaplocksActive;
}
