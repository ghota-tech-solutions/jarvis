// Enter to submit on the dashboard form too.
document.querySelectorAll('textarea[name="goal"]').forEach((ta) => {
  ta.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {
      e.preventDefault();
      if (typeof ta.form.requestSubmit === 'function') ta.form.requestSubmit();
      else ta.form.submit();
    }
  });
});
