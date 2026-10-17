import js from '@eslint/js'
import globals from 'globals'
import tseslint from 'typescript-eslint'
import pluginVue from 'eslint-plugin-vue'
import { defineConfig } from 'eslint/config'

export default defineConfig([
  {
    ignores: [
      'dist/**',
      'src-tauri/**',
      'crates/**',
      'target/**',
      'src/bindings.ts',
      'src/vite-env.d.ts',
      'src/auto-imports.d.ts',
      'src/components.d.ts',
      'auto-imports.d.ts',
      'components.d.ts',
    ],
  },
  { files: ['**/*.{js,mjs,cjs,ts,vue}'], plugins: { js }, extends: ['js/recommended'] },
  { files: ['**/*.{js,mjs,cjs,ts,vue}'], languageOptions: { globals: globals.browser } },
  tseslint.configs.recommended,
  pluginVue.configs['flat/essential'],
  {
    files: ['**/*.vue'],
    languageOptions: {
      parserOptions: {
        parser: tseslint.parser,
        ecmaFeatures: {
          jsx: true,
        },
      },
    },
  },
])
