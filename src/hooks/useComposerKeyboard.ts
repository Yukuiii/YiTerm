import { useRef, type KeyboardEvent } from 'react';

/** Enter confirms IME candidates before it can submit either composer. */
export function useComposerKeyboard(onSubmit: () => void | Promise<void>) {
  const composing = useRef(false);

  return {
    onCompositionStart: () => { composing.current = true; },
    onCompositionEnd: () => { composing.current = false; },
    onKeyDown: (event: KeyboardEvent<HTMLTextAreaElement>) => {
      // WebKit may end composition before the confirming keydown and report
      // isComposing=false. Its legacy IME keycode still identifies that Enter.
      if (event.key !== 'Enter' || event.shiftKey || event.defaultPrevented
        || composing.current || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return;
      event.preventDefault();
      void onSubmit();
    },
  };
}
