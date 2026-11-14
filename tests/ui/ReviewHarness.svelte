<script lang="ts">
  import ConflictReview from '../../src/lib/components/ConflictReview.svelte';
  import type { ConflictReviewModel } from '../../src/lib/utils/conflict-review';
  import type { ReviewState } from '../../src/lib/utils/conflict-review';
  let { models }: { models: ConflictReviewModel[] } = $props();
  let states = $state<ReviewState[]>([]);
  let active = $state(0);
  let focus = $state<() => boolean>(() => false);
  let render = $state<(body:string) => string>(() => '');
  $effect(() => {
    const stops = models.map((model,index) => model.subscribe(state => { states[index] = state; }));
    return () => stops.forEach(stop=>stop());
  });
  export function activate(index:number) { active=index;focus=()=>active===index; }
  export function renderer(marker:string) { render=body=>`<p>${marker}: ${body.replaceAll('&','&amp;').replaceAll('<','&lt;')}</p>`; }
</script>
{#each models as model,index}
  {#if states[index]?.visible}
    <div data-test-pane={index}>
      <ConflictReview {model} ownerId={`pane-${index}`} ownsFocus={() => active===index} focusIntent={focus}
        onApply={async()=>{await model.apply();}} onKeepBoth={async()=>{}} renderSnapshot={render}/>
    </div>
  {/if}
{/each}
