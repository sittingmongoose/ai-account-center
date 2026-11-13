/**
 * CCS Type Definitions
 * Single entry point for all types
 */

// Config types
export type {
  Config,
  ProfilesConfig,
  Settings,
  EnvVars,
  EnvValue,
  ProfileMetadata,
  ProfilesRegistry,
  CLIProxyVariantConfig,
  CLIProxyVariantsConfig,
  OpenAICompatProxyConfig,
  OpenAICompatProxyRoutingConfig,
} from './config';
export { isConfig, isSettings } from './config';

// CLI types
export type { ParsedArgs, ClaudeSpawnOptions, Platform, ClaudeCliInfo } from './cli';
export { ExitCode } from './cli';

// Legacy GLMT transformer types
export type {
  AnthropicMessage,
  ContentBlock,
  AnthropicRequest,
  AnthropicTool,
  OpenAIMessage,
  OpenAIToolCall,
  OpenAIRequest,
  OpenAITool,
  SSEEvent,
  DeltaChunk,
  TransformationContext,
} from './glmt';

// Utility types
export { LogLevel } from './utils';
export type { ErrorCode, ColorName, TerminalInfo, Result } from './utils';

// Profile routing types
export type { ProfileType } from './profile';
