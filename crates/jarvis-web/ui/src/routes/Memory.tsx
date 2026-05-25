import type { Component } from 'solid-js';
import MemoryList from '~/features/memory/MemoryList';
import AppErrorBoundary from '~/components/ErrorBoundary';

const Memory: Component = () => {
  return (
    <AppErrorBoundary name="Memory">
      <MemoryList />
    </AppErrorBoundary>
  );
};

export default Memory;
