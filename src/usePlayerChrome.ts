import { useEffect, useState } from 'react';

const IDLE_DELAY = 3000;
const CHROME = '.app-header, .transport, .status-bar';

export function usePlayerChrome(fullscreen: boolean, keepVisible: boolean) {
  const [idle, setIdle] = useState(false);

  useEffect(() => {
    setIdle(false);
    if (!fullscreen || keepVisible) return;

    let timer: ReturnType<typeof setTimeout>;
    let pointerDown = false;
    let keyboard = false;
    const reveal = () => {
      setIdle(false);
      clearTimeout(timer);
      timer = setTimeout(() => {
        // Keep sliders visible throughout a drag, and retain controls while
        // someone is navigating them with the keyboard.
        if (pointerDown || (keyboard && document.activeElement?.closest(CHROME))) return;
        setIdle(true);
      }, IDLE_DELAY);
    };
    const move = () => { keyboard = false; reveal(); };
    const press = () => { pointerDown = true; keyboard = false; reveal(); };
    const release = () => { pointerDown = false; reveal(); };
    const key = () => { keyboard = true; reveal(); };
    const focus = () => { if (keyboard) reveal(); };

    window.addEventListener('pointermove', move, true);
    window.addEventListener('pointerdown', press, true);
    window.addEventListener('pointerup', release, true);
    window.addEventListener('pointercancel', release, true);
    window.addEventListener('keydown', key, true);
    window.addEventListener('wheel', move, { passive: true, capture: true });
    window.addEventListener('focusin', focus, true);
    window.addEventListener('focusout', focus, true);
    window.addEventListener('blur', release);
    reveal();

    return () => {
      clearTimeout(timer);
      window.removeEventListener('pointermove', move, true);
      window.removeEventListener('pointerdown', press, true);
      window.removeEventListener('pointerup', release, true);
      window.removeEventListener('pointercancel', release, true);
      window.removeEventListener('keydown', key, true);
      window.removeEventListener('wheel', move, true);
      window.removeEventListener('focusin', focus, true);
      window.removeEventListener('focusout', focus, true);
      window.removeEventListener('blur', release);
    };
  }, [fullscreen, keepVisible]);

  return fullscreen && !keepVisible && idle;
}
