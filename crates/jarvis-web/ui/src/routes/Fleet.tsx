import type { Component } from 'solid-js';
import FleetDag from '~/features/fleet/FleetDag';
import AppErrorBoundary from '~/components/ErrorBoundary';

const Fleet: Component = () => {
  return (
    <AppErrorBoundary name="Fleet">
      <FleetDag />
    </AppErrorBoundary>
  );
};

export default Fleet;
