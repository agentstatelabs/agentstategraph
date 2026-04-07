// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
	site: 'https://agentstategraph.dev',
	integrations: [
		starlight({
			title: 'AgentStateGraph',
			social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/agentstatelabs/AgentStateGraph' }],
			customCss: ['./src/styles/custom.css'],
			sidebar: [
				{
					label: 'Getting Started',
					items: [
						{ label: 'Introduction', slug: 'guides/introduction' },
						{ label: 'Quick Start', slug: 'guides/quickstart' },
						{ label: 'Core Concepts', slug: 'guides/concepts' },
					],
				},
				{
					label: 'Language Guides',
					items: [
						{ label: 'MCP Server', slug: 'guides/mcp-server' },
						{ label: 'Python', slug: 'guides/python' },
						{ label: 'TypeScript', slug: 'guides/typescript' },
						{ label: 'Go', slug: 'guides/go' },
						{ label: 'WASM / Browser', slug: 'guides/wasm' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ label: 'MCP Tools (20)', slug: 'reference/mcp-tools' },
						{ label: 'RFC Specification', slug: 'reference/rfc' },
					],
				},
				{
					label: 'Blog',
					items: [
						{ label: 'The Missing Primitive', slug: 'blog/the-missing-primitive' },
					],
				},
			],
		}),
	],
});
