import { useParams } from '@solidjs/router';
import type { Component } from 'solid-js';

const Task: Component = () => {
  const params = useParams<{ id: string }>();
  return (
    <section>
      <h1 style="color: var(--heading)">Task</h1>
      <p>
        <span class="dim">id:</span> <code>{params.id}</code>
      </p>
      <p class="dim">Transcript + scrubbable timeline — wired in M6.S10 / M6.S12.</p>
    </section>
  );
};

export default Task;
