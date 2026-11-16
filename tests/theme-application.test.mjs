import test from 'node:test';
import assert from 'node:assert/strict';
import { applyTheme } from '../src/lib/utils/theme.ts';

function rootFixture() {
  const classes=new Set(), attributes=new Map(), styles=new Map();
  return { classes, attributes, styles,
    classList:{ add:value=>classes.add(value), remove:value=>classes.delete(value) },
    setAttribute:(key,value)=>attributes.set(key,value), removeAttribute:key=>attributes.delete(key),
    style:{ setProperty:(key,value)=>styles.set(key,value), removeProperty:key=>styles.delete(key) } };
}

test('custom theme startup and cancellation use the same variables and clear them for named themes', () => {
  const root=rootFixture();
  const custom={id:'custom-fixture',is_dark:true,colors:{bg_primary:'#111',bg_secondary:'#222',bg_tertiary:'#333',bg_hover:'#444',bg_active:'#555',bg_editor:'#666',text_primary:'#fff',text_secondary:'#ccc',border_color:'#888'}};
  applyTheme(custom.id,[custom],root);
  assert.equal(root.styles.get('--border-light'),'#888');
  assert.equal(root.styles.get('--text-tertiary'),'#ccc');
  assert.ok(root.classes.has('dark'));
  applyTheme('github-light',[custom],root);
  assert.equal(root.styles.size,0);
  assert.equal(root.attributes.get('data-theme'),'github-light');
  assert.equal(root.classes.has('dark'),false);
  applyTheme('dark',[],root);
  assert.equal(root.attributes.has('data-theme'),false);
  assert.ok(root.classes.has('dark'));
});
