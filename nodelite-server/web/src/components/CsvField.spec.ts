import { describe, expect, it } from 'vitest';
import { mount } from '@vue/test-utils';
import CsvField from './CsvField.vue';

function mountField(modelValue: string[]) {
  return mount(CsvField, { props: { modelValue, 'onUpdate:modelValue': () => {} } });
}

describe('CsvField', () => {
  it('renders the array joined as comma-separated text', () => {
    const wrapper = mountField(['a', 'b', 'c']);
    expect((wrapper.find('input').element as HTMLInputElement).value).toBe('a, b, c');
  });

  it('emits a trimmed, blank-filtered array on input', async () => {
    const wrapper = mountField([]);
    await wrapper.find('input').setValue(' x , , y ,z ');
    const emitted = wrapper.emitted('update:modelValue');
    expect(emitted?.at(-1)?.[0]).toEqual(['x', 'y', 'z']);
  });

  it('keeps the raw text while typing a trailing comma (not clobbered)', async () => {
    const wrapper = mountField(['a']);
    const input = wrapper.find('input');
    await input.setValue('a, ');
    // model normalizes to ['a'], but the visible text must keep the trailing comma
    expect(wrapper.emitted('update:modelValue')?.at(-1)?.[0]).toEqual(['a']);
    expect((input.element as HTMLInputElement).value).toBe('a, ');
  });

  it('resyncs the text when the model changes externally', async () => {
    const wrapper = mountField(['a']);
    await wrapper.setProps({ modelValue: ['p', 'q'] });
    expect((wrapper.find('input').element as HTMLInputElement).value).toBe('p, q');
  });
});
