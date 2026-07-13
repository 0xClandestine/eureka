# Expert-in-the-Loop Workflow

Scientists can interact with a running co-scientist session via the CLI:

1. **Pause the session:**
   ```
   eureka session pause <session-id>
   ```

2. **Inject an expert hypothesis:**
   ```
   eureka session inject <session-id> generation in \
     '{"hypotheses":[{"statement":"...","rationale":"...","assumptions":["..."],"testable_predictions":["..."],"proposed_experiment":"..."}]}'
   ```

3. **Inject an expert review:**
   ```
   eureka session inject <session-id> reflection in \
     '{"reviews":[{"hypothesis":{...},"kind":"expert","score":9,"reasoning":"..."}]}'
   ```

4. **Resume the session:**
   ```
   eureka session resume <session-id>
   ```

Expert-injected content carries authoritative weight — agents treat it as high-priority input and surface any conflicts explicitly.