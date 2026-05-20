import { For, Show, createMemo, type Component } from 'solid-js';
import { createQuery } from '@tanstack/solid-query';
import { costBreakdownQuery } from '~/lib/api/queries';

type Props = {
  taskId: string;
};

const Telemetry: Component<Props> = (p) => {
  const costQ = createQuery(() => costBreakdownQuery(p.taskId));

  const totalCost = createMemo(() => costQ.data?.totalCostUsd ?? 0);
  const totalIn = createMemo(() => Number(costQ.data?.totalTokensIn ?? 0n));
  const totalOut = createMemo(() => Number(costQ.data?.totalTokensOut ?? 0n));
  const totalTokens = createMemo(() => totalIn() + totalOut());

  // Budgets soft limits
  const DOLLAR_BUDGET_SOFT_LIMIT = 5.0; // $5 USD soft budget
  const TOKEN_BUDGET_SOFT_LIMIT = 500000; // 500,000 tokens soft budget

  const dollarPercentage = createMemo(() => 
    Math.min(100, Math.round((totalCost() / DOLLAR_BUDGET_SOFT_LIMIT) * 100))
  );

  const tokenPercentage = createMemo(() => 
    Math.min(100, Math.round((totalTokens() / TOKEN_BUDGET_SOFT_LIMIT) * 100))
  );

  // SVG Circle Gauge calculations
  const RADIUS = 40;
  const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

  const dollarStrokeOffset = createMemo(() => 
    CIRCUMFERENCE - (dollarPercentage() / 100) * CIRCUMFERENCE
  );

  const tokenStrokeOffset = createMemo(() => 
    CIRCUMFERENCE - (tokenPercentage() / 100) * CIRCUMFERENCE
  );

  return (
    <section>
      <header style="margin-bottom: 1.2rem">
        <h3 class="heading" style="margin: 0">Model Telemetry & Budget</h3>
        <p class="dim" style="margin: 0.15rem 0; font-size: 11px">
          Live cost and model execution analysis for this task chain
        </p>
      </header>

      <Show when={costQ.data} fallback={<p class="dim">loading cost statistics…</p>}>
        <div class="telemetry-grid">
          {/* Circular Dollar Cost Gauge */}
          <div class="telemetry-card">
            <span class="telemetry-card-title">Dollar Cost Spend</span>
            <div class="progress-container">
              <svg width="100" height="100" viewBox="0 0 100 100">
                <circle
                  class="progress-ring-circle-bg"
                  cx="50"
                  cy="50"
                  r={RADIUS}
                  stroke-width="8"
                />
                <circle
                  class="progress-ring-circle"
                  cx="50"
                  cy="50"
                  r={RADIUS}
                  stroke-width="8"
                  stroke={dollarPercentage() > 80 ? 'var(--error)' : 'var(--accent)'}
                  stroke-dasharray={String(CIRCUMFERENCE)}
                  stroke-dashoffset={String(dollarStrokeOffset())}
                />
              </svg>
              <div class="telemetry-percentage">{dollarPercentage()}%</div>
            </div>
            <div class="telemetry-card-value">${totalCost().toFixed(4)}</div>
            <span class="dim" style="font-size: 10px; margin-top: 0.15rem">
              soft limit: ${DOLLAR_BUDGET_SOFT_LIMIT.toFixed(2)}
            </span>
          </div>

          {/* Circular Token Spend Gauge */}
          <div class="telemetry-card">
            <span class="telemetry-card-title">Tokens Consumed</span>
            <div class="progress-container">
              <svg width="100" height="100" viewBox="0 0 100 100">
                <circle
                  class="progress-ring-circle-bg"
                  cx="50"
                  cy="50"
                  r={RADIUS}
                  stroke-width="8"
                />
                <circle
                  class="progress-ring-circle"
                  cx="50"
                  cy="50"
                  r={RADIUS}
                  stroke-width="8"
                  stroke={tokenPercentage() > 80 ? 'var(--warn)' : 'var(--assistant)'}
                  stroke-dasharray={String(CIRCUMFERENCE)}
                  stroke-dashoffset={String(tokenStrokeOffset())}
                />
              </svg>
              <div class="telemetry-percentage">{tokenPercentage()}%</div>
            </div>
            <div class="telemetry-card-value">{(totalTokens() / 1000).toFixed(1)}k</div>
            <span class="dim" style="font-size: 10px; margin-top: 0.15rem">
              in: {(totalIn() / 1000).toFixed(1)}k · out: {(totalOut() / 1000).toFixed(1)}k
            </span>
          </div>
        </div>

        {/* Model Spends Detail Table */}
        <h4 class="heading" style="margin: 1.5rem 0 0.5rem 0; font-size: 13px">
          Model Utilization Details
        </h4>
        <Show when={costQ.data!.byModel.length > 0} fallback={
          <p class="dim" style="font-style: italic; font-size: 12px">
            No model spends logged yet.
          </p>
        }>
          <table class="telemetry-table">
            <thead>
              <tr>
                <th>Model</th>
                <th>Calls</th>
                <th>Tokens In</th>
                <th>Tokens Out</th>
                <th>USD Cost</th>
              </tr>
            </thead>
            <tbody>
              <For each={costQ.data!.byModel}>
                {(m) => (
                  <tr>
                    <td style="font-weight: 600; color: var(--assistant)">
                      {m.modelName}
                    </td>
                    <td>{m.callCount}</td>
                    <td>{Number(m.tokensIn).toLocaleString()}</td>
                    <td>{Number(m.tokensOut).toLocaleString()}</td>
                    <td style="font-family: monospace; font-weight: bold; color: var(--heading)">
                      ${m.costUsd.toFixed(4)}
                    </td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </Show>
      </Show>
    </section>
  );
};

export default Telemetry;
