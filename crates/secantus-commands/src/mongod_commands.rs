//! mongod's own table of its commands: what `listCommands` answers.
//!
//! GENERATED from `listCommands` on mongod 8.2.11 (2026-10-13), less the
//! internal commands whose names start with `_`. Do not edit a row by hand:
//! re-run the command against the build being targeted. The table says which
//! commands run only against `admin`, which are in API Version 1, and what
//! each one's help text is, so those three answers are mongod's and not a
//! list kept beside it.

#![cfg_attr(rustfmt, rustfmt_skip)]

pub(crate) const ADMIN_ONLY: u8 = 1;
pub(crate) const SECONDARY_OK: u8 = 2;
pub(crate) const REQUIRES_AUTH: u8 = 4;
pub(crate) const API_V1: u8 = 8;
pub(crate) const SECONDARY_OVERRIDE_OK: u8 = 16;

/// `(name, flags, help)`, sorted by name.
pub(crate) const COMMANDS: &[(&str, u8, &str)] = &[
    (
        "abortTransaction",
        ADMIN_ONLY | REQUIRES_AUTH | API_V1,
        "Aborts a transaction",
    ),
    (
        "aggregate",
        REQUIRES_AUTH | API_V1 | SECONDARY_OVERRIDE_OK,
        "Runs the aggregation command. See http://dochub.mongodb.org/core/aggregation for more details.",
    ),
    (
        "analyzeShardKey",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Returns metrics for evaluating a shard key for a collection.",
    ),
    (
        "appendOplogNote",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Adds a no-op entry to the oplog",
    ),
    (
        "applyOps",
        REQUIRES_AUTH,
        "internal command to apply oplog entries\n{ applyOps : [ ] }",
    ),
    (
        "authenticate",
        SECONDARY_OK | API_V1,
        "no help defined",
    ),
    (
        "autoCompact",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "enable auto compaction for a database\nwarning: compact operation has blocking behaviour and is slow, enabling auto compact will allow compact to run on any collection at any time. You can cancel by disabling auto compact.\n{ autoCompact : <bool>, [freeSpaceTargetMB:<int64_t>], [runOnce:<bool>] }\n  freeSpaceTargetMB - minimum amount of space recoverable for compaction to proceed\n  runOnce - executes compaction on the database only once\n",
    ),
    (
        "autoSplitVector",
        REQUIRES_AUTH,
        "Internal command returning the split points for a chunk, given the maximum chunk size.",
    ),
    (
        "buildInfo",
        SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "bulkWrite",
        ADMIN_ONLY | REQUIRES_AUTH | API_V1,
        "command to apply inserts, updates and deletes in bulk",
    ),
    (
        "checkShardingIndex",
        REQUIRES_AUTH,
        "Internal command.\n",
    ),
    (
        "cleanupOrphaned",
        ADMIN_ONLY | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "cleanupStructuredEncryptionData",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "cloneCollectionAsCapped",
        REQUIRES_AUTH,
        "{ cloneCollectionAsCapped:<fromName>, toCollection:<toName>, size:<sizeInBytes> }",
    ),
    (
        "clusterAbortTransaction",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Aborts a transaction",
    ),
    (
        "clusterAggregate",
        SECONDARY_OK | REQUIRES_AUTH,
        "Runs the sharded aggregation command. See http://dochub.mongodb.org/core/aggregation for more details.",
    ),
    (
        "clusterBulkWrite",
        ADMIN_ONLY | REQUIRES_AUTH,
        "command to apply inserts, updates and deletes in bulk",
    ),
    (
        "clusterCommitTransaction",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Commits a transaction",
    ),
    (
        "clusterCount",
        SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "clusterDelete",
        REQUIRES_AUTH,
        "delete documents",
    ),
    (
        "clusterFind",
        REQUIRES_AUTH | SECONDARY_OVERRIDE_OK,
        "query for documents",
    ),
    (
        "clusterGetMore",
        SECONDARY_OK | REQUIRES_AUTH,
        "retrieve more documents for a cursor id",
    ),
    (
        "clusterInsert",
        REQUIRES_AUTH,
        "insert documents",
    ),
    (
        "clusterUpdate",
        REQUIRES_AUTH,
        "update documents",
    ),
    (
        "collMod",
        REQUIRES_AUTH | API_V1,
        "Sets collection options.\nExample: { collMod: 'foo', viewOn: 'bar'} Example: { collMod: 'foo', index: {keyPattern: {a: 1}, expireAfterSeconds: 600} Example: { collMod: 'foo', index: {name: 'bar', expireAfterSeconds: 600} }\n",
    ),
    (
        "collStats",
        SECONDARY_OK | REQUIRES_AUTH,
        "{ collStats:\"blog.posts\" , scale : 1 } scale divides sizes e.g. for KB use 1024.\navgObjSize - in bytes",
    ),
    (
        "commitTransaction",
        ADMIN_ONLY | REQUIRES_AUTH | API_V1,
        "Commits a transaction",
    ),
    (
        "compact",
        SECONDARY_OK | REQUIRES_AUTH,
        "compact collection\nwarning: this operation has blocking behaviour and is slow. You can cancel with killOp()\n{ compact : <collection_name>, [dryRun:<bool>], [force:<bool>], [freeSpaceTargetMB:<int64_t>] }\n  dryRun - runs only the estimation phase of the compact operation\n  force - allows to run on a replica set primary\n  freeSpaceTargetMB - minimum amount of space recoverable for compaction to proceed\n",
    ),
    (
        "compactStructuredEncryptionData",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "configureQueryAnalyzer",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Starts or stops collecting metrics about read and write queries against a collection.",
    ),
    (
        "connPoolStats",
        SECONDARY_OK | REQUIRES_AUTH,
        "stats about connections between servers in a replica set or sharded cluster.",
    ),
    (
        "connPoolSync",
        SECONDARY_OK | REQUIRES_AUTH,
        "internal",
    ),
    (
        "connectionStatus",
        SECONDARY_OK,
        "no help defined",
    ),
    (
        "convertToCapped",
        REQUIRES_AUTH,
        "{ convertToCapped:<fromCollectionName>, size:<sizeInBytes> }",
    ),
    (
        "coordinateCommitTransaction",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Coordinates the commit for a transaction. Only called by mongos.",
    ),
    (
        "count",
        REQUIRES_AUTH | API_V1 | SECONDARY_OVERRIDE_OK,
        "count objects in collection",
    ),
    (
        "create",
        REQUIRES_AUTH | API_V1,
        "explicitly creates a collection or view\n{\n  create: <string: collection or view name> [,\n  capped: <bool: capped collection>,\n  idIndex: <document: _id index specification>,\n  size: <int: size in bytes of the capped collection>,\n  max: <int: max number of documents in the capped collection>,\n  storageEngine: <document: storage engine configuration>,\n  validator: <document: validation rules>,\n  validationLevel: <string: validation level>,\n  validationAction: <string: validation action>,\n  indexOptionDefaults: <document: default configuration for indexes>,\n  viewOn: <string: name of source collection or view>,\n  pipeline: <array<object>: aggregation pipeline stage>,\n  collation: <document: default collation for the collection or view>,\n  changeStreamPreAndPostImages: <document: pre- and post-images options for change streams>,\n  writeConcern: <document: write concern expression for the operation>]\n}",
    ),
    (
        "createIndexes",
        REQUIRES_AUTH | API_V1,
        "no help defined",
    ),
    (
        "createRole",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "createSearchIndexes",
        REQUIRES_AUTH,
        "Command to create a search index. Only supported with Atlas.",
    ),
    (
        "createUser",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "currentOp",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "dataSize",
        SECONDARY_OK | REQUIRES_AUTH,
        "Determine data size for a set of data in a certain range.\nExample: { dataSize:\"blog.posts\", keyPattern:{x:1}, min:{x:10}, max:{x:55} }\n{min} and {max} parameters are optional. They must either both be included or both omitted.\n{keyPattern} is an optional parameter indicating an index pattern that would be useful for iterating over the min/max bounds.\nIf keyPattern is omitted, it is inferred from the structure of min.\nNote: This command may take a while to run.",
    ),
    (
        "dbCheck",
        REQUIRES_AUTH,
        "Validate replica set consistency.\nInvoke with { dbCheck: <collection name/uuid>,\n              minKey: <first key, exclusive>,\n              maxKey: <last key, inclusive>,\n              maxCount: <try to keep a batch within maxCount number of docs>,\n              maxSize: <try to keep a batch withing maxSize of docs (bytes)>,\n              maxDocsPerBatch: <max number of docs/batch>\n              maxBatchTimeMillis: <max time processing a batch in milliseconds>\nto check a collection.\nInvoke with {dbCheck: 1} to check all collections in the database.",
    ),
    (
        "dbHash",
        SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "dbStats",
        SECONDARY_OK | REQUIRES_AUTH,
        "Get stats on a database. Not instantaneous.\nSlower for databases with large .ns files.\nExample: { dbStats:1, scale:1 }",
    ),
    (
        "delete",
        REQUIRES_AUTH | API_V1,
        "delete documents",
    ),
    (
        "distinct",
        REQUIRES_AUTH | SECONDARY_OVERRIDE_OK,
        "{ distinct : 'collection name' , key : 'a.b' , query : {} }",
    ),
    (
        "drop",
        REQUIRES_AUTH | API_V1,
        "drop a collection\n{drop : <collectionName>}",
    ),
    (
        "dropAllRolesFromDatabase",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "dropAllUsersFromDatabase",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "dropConnections",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Drop egress connections to specified host and port",
    ),
    (
        "dropDatabase",
        REQUIRES_AUTH | API_V1,
        "drop (delete) this database",
    ),
    (
        "dropIndexes",
        REQUIRES_AUTH | API_V1,
        "drop indexes for a collection",
    ),
    (
        "dropRole",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "dropSearchIndex",
        REQUIRES_AUTH,
        "Command to drop a search index. Only supported with Atlas.",
    ),
    (
        "dropUser",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "endSessions",
        SECONDARY_OK | REQUIRES_AUTH | API_V1,
        "end a set of logical sessions",
    ),
    (
        "explain",
        REQUIRES_AUTH | API_V1 | SECONDARY_OVERRIDE_OK,
        "explain database reads and writes",
    ),
    (
        "features",
        SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "filemd5",
        SECONDARY_OK | REQUIRES_AUTH,
        " example: { filemd5 : ObjectId(aaaaaaa) , root : \"fs\" }",
    ),
    (
        "find",
        REQUIRES_AUTH | API_V1 | SECONDARY_OVERRIDE_OK,
        "query for documents",
    ),
    (
        "findAndModify",
        REQUIRES_AUTH | API_V1,
        "{ findAndModify: \"collection\", query: {processed:false}, update: {$set: {processed:true}}, new: true}\n{ findAndModify: \"collection\", query: {processed:false}, remove: true, sort: {priority:-1}}\nEither update or remove is required, all other fields have default values.\nOutput is in the \"value\" field\n",
    ),
    (
        "flushRouterConfig",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Flushes the cached routing information for a single collection, entire database (and its collections) or all databases, which would cause full reload from the config server on the next access.\nUsage:\n{flushRouterConfig: 1} flushes all databases\n{flushRouterConfig: 'db'} flushes only the given database (and its collections)\n{flushRouterconfig: 'db.coll'} flushes only the given collection",
    ),
    (
        "fsync",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "http://dochub.mongodb.org/core/fsynccommand",
    ),
    (
        "fsyncUnlock",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "getChangeStreamState",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Gets the change stream state in the serverless\nUsage:\n    {getChangeStreamState: 1}",
    ),
    (
        "getClusterParameter",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Get in-memory cluster parameter value(s) from this node",
    ),
    (
        "getCmdLineOpts",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "getDatabaseVersion",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        " example: { getDatabaseVersion : 'foo'  } ",
    ),
    (
        "getDefaultRWConcern",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "get the current read/write concern defaults being applied by this node",
    ),
    (
        "getDiagnosticData",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "get latest diagnostic data collection snapshot",
    ),
    (
        "getLog",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "{ getLog : '*' }  OR { getLog : 'global' }",
    ),
    (
        "getMore",
        SECONDARY_OK | REQUIRES_AUTH | API_V1,
        "retrieve more results from an existing cursor",
    ),
    (
        "getParameter",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "get administrative option(s)\nexample:\n{ getParameter:1, notablescan:1 }\npass a document as the value for getParameter to request options\nexample:\n{ getParameter:{showDetails: true}, notablescan:1}\nsupported:\n  AbortExpiredTransactionsSessionCheckoutTimeout\n  AbortOldestTransactionMemoryClearLimitPerBatch\n  AbortOldestTransactionSessionCheckoutTimeoutMilliseconds\n  AbortOldestTransactionSessionKillLimitPerBatch\n  AlwaysRecordTraffic\n  KeysRotationIntervalSec\n  KillSessionsMaxConcurrency\n  KillSessionsPerHostTimeoutMS\n  ShardingTaskExecutorPoolHostTimeoutMS\n  ShardingTaskExecutorPoolMaxConnecting\n  ShardingTaskExecutorPoolMaxQueueDepth\n  ShardingTaskExecutorPoolMaxSize\n  ShardingTaskExecutorPoolMaxSizeForConfigServers\n  ShardingTaskExecutorPoolMinSize\n  ShardingTaskExecutorPoolMinSizeForConfigServers\n  ShardingTaskExecutorPoolRefreshRequirementMS\n  ShardingTaskExecutorPoolRefreshTimeoutMS\n  ShardingTaskExecutorPoolReplicaSetMatching\n  TransactionRecordMinimumLifetimeMinutes\n  acceptApiVersion2\n  allowDiskUseByDefault\n  allowDocumentsGreaterThanMaxUserSize\n  allowEmptyOplogBatchesToPropagateCommitPoint\n  allowMultipleArbiters\n  allowRolesFromX509Certificates\n  allowUnsafeUntimestampedWrites\n  analyzeShardKeyCharacteristicsDefaultSampleSize\n  analyzeShardKeyMonotonicityCorrelationCoefficientThreshold\n  analyzeShardKeyNumMostCommonValues\n  analyzeShardKeyNumRanges\n  analyzeShardKeyNumSamplesPerRange\n  analyzeShardKeySplitPointExpirationSecs\n  authSchemaVersion\n  authenticationMechanisms\n  authorizationManagerCacheSize\n  autoMergerIntervalSecs\n  autoMergerMaxTimeProcessingChunksMS\n  autoMergerThrottlingMS\n  automationServiceDescriptor\n  awsEC2InstanceMetadataUrl\n  awsECSInstanceMetadataUrl\n  backtraceLogFile\n  balancerChunksSelectionTimeoutMs\n  balancerMigrationsThrottlingMs\n  batchUserMultiDeletes\n  batchedDeletesTargetBatchDocs\n  batchedDeletesTargetBatchTimeMS\n  batchedDeletesTargetStagedDocBytes\n  bgSyncOplogFetcherBatchSize\n  bsonTestValidationVersion\n  buildInfoAuthMode\n  bulkWriteMaxRepliesSize\n  bypassRankFusionFCVGate\n  cachePressureEvictionStallDetectionWindowSeconds\n  cachePressureEvictionStallThresholdProportion\n  cachePressureExponentiallyDecayingMovingAverageAlphaValue\n  cachePressureQueryPeriodMilliseconds\n  capMemoryConsumptionForPreAuthBuffers\n  catalogCacheCollectionMaxEntries\n  catalogCacheDatabaseMaxEntries\n  changeCollectionExpiredDocumentsRemoverJobSleepSeconds\n  changeSyncSourceThresholdMillis\n  chunkDefragmentationThrottlingMS\n  chunkMigrationConcurrency\n  chunkMigrationFetcherMaxBufferedSizeBytesPerThread\n  clientCursorMonitorFrequencySecs\n  clientSourceAuthenticationRestrictionMode\n  clusterAuthMode\n  clusterIpSourceAllowlist\n  clusterServerParameterRefreshIntervalSecs\n  collectionBulkLoaderBatchSizeInBytes\n  collectionClonerBatchSize\n  collectionClonerUsesExhaust\n  collectionSamplingLogIntervalSeconds\n  connPoolMaxConnsPerHost\n  connPoolMaxInUseConnsPerHost\n  connectTimeoutMs\n  coordinateCommitReturnImmediatelyAfterPersistingDecision\n  createRollbackDataFiles\n  cursorTimeoutMillis\n  dbCheckHealthLogEveryNBatches\n  dbCheckMaxConsecutiveIdenticalIndexKeysPerSnapshot\n  dbCheckMaxInternalRetries\n  dbCheckMaxRunsOnQueue\n  dbCheckMaxTotalIndexKeysPerSnapshot\n  dbCheckSecondaryBatchMaxTimeMs\n  defaultConfigCommandTimeoutMS\n  defaultFindReplicaSetHostTimeoutMS\n  defaultStartupFCV\n  delinquentAcquisitionIntervalMillis\n  diagnosticDataCollectionDirectoryPath\n  diagnosticDataCollectionDirectorySizeMB\n  diagnosticDataCollectionDiscardLargeSamples\n  diagnosticDataCollectionEnableLatencyHistograms\n  diagnosticDataCollectionEnabled\n  diagnosticDataCollectionFileSizeMB\n  diagnosticDataCollectionMaxThreads\n  diagnosticDataCollectionMetadataCaptureFrequency\n  diagnosticDataCollectionMinThreads\n  diagnosticDataCollectionPeriodMillis\n  diagnosticDataCollectionSampleTimeoutMillis\n  diagnosticDataCollectionSamplesPerChunk\n  diagnosticDataCollectionSamplesPerInterimUpdate\n  diagnosticDataCollectionStatsNamespaces\n  diagnosticDataCollectionVerboseTCMalloc\n  directConnectionChecksWithSingleShard\n  disableExpiredChangeCollectionRemover\n  disableExpiredPreImagesRemover\n  disableLogicalSessionCacheRefresh\n  disableNonSSLConnectionLogging\n  disableNonTLSConnectionLogging\n  disableResumableRangeDeleter\n  disableSplitHorizonIPCheck\n  disableTransitionFromLatestToLastContinuous\n  disabledSecureAllocatorDomains\n  disaggregatedStorageEnabled\n  enableAccessToUserRoles\n  enableAutoCompaction\n  enableCompoundWildcardIndexLog\n  enableComputeMode\n  enableDefaultWriteConcernUpdatesForInitiate\n  enableDetailedConnectionHealthMetricLogLines\n  enableDiagnosticLogging\n  enableElectionHandoff\n  enableFinerGrainedCatalogCacheRefresh\n  enableFlowControl\n  enableIndexBuildCommitQuorum\n  enableLocalhostAuthBypass\n  enableOverrideClusterChainingSetting\n  enableQueryKilledByRangeDeletionLog\n  enableReconfigRollbackCommittedWritesCheck\n  enableSearchMeta\n  enableShardedIndexConsistencyCheck\n  enableTemporarilyUnavailableExceptions\n  enableTestCommands\n  enableTimeoutOfInactiveSessionCursors\n  enforceUserClusterSeparation\n  expiredChangeStreamPreImageRemovalJobSleepSecs\n  externalPipeDir\n  fassertOnLockTimeoutForStepUpDown\n  featureCompatibilityVersion\n  featureFlag80CollectionCreationPath\n  featureFlagAccuratePercentiles\n  featureFlagAggMongosToRouter\n  featureFlagAggOutTimeseries\n  featureFlagAllBinariesSupportRawDataOperations\n  featureFlagAllMongodsAreSharded\n  featureFlagAllowAdditionalParticipants\n  featureFlagAllowMergeOnNullishValues\n  featureFlagAlwaysCreateConfigTransactionsPartialIndexOnStepUp\n  featureFlagArrayAccumulators\n  featureFlagAutoCompact\n  featureFlagBalanceUnshardedCollections\n  featureFlagBalancerWindowDOW\n  featureFlagBanEncryptionOptionsInCollectionCreation\n  featureFlagBinDataConvert\n  featureFlagBinDataConvertNumeric\n  featureFlagBlender\n  featureFlagBulkWriteCommand\n  featureFlagChangeStreamPreciseShardTargeting\n  featureFlagCheckInvalidDatabaseInGlobalCatalog\n  featureFlagCheckRangeDeletionsWithMissingShardKeyIndex\n  featureFlagClusterFsyncLock\n  featureFlagClusterWideToaster\n  featureFlagClusteredConfigTransactions\n  featureFlagCompactOptions\n  featureFlagConcurrencyInChunkMigration\n  featureFlagConfigDebugDumpSupported\n  featureFlagCreateCollectionInPreparedTransactions\n  featureFlagCreateDatabaseDDLCoordinator\n  featureFlagCreateSpillKVEngine\n  featureFlagCreateViewlessTimeseriesCollections\n  featureFlagCurrentDate\n  featureFlagCursorBasedTop\n  featureFlagDatabaseDualCatalogCache\n  featureFlagDistinguishMetadataInconsistencyFromConflictingOperation\n  featureFlagDocumentSourceListCatalog\n  featureFlagDropIndexesDDLCoordinator\n  featureFlagDualCatalogCache\n  featureFlagEnableReplicasetTransitionToCSRS\n  featureFlagEndOfTransactionChangeEvent\n  featureFlagErrorAndLogValidationAction\n  featureFlagExposeClientIpInAuditLogs\n  featureFlagExtensionsAPI\n  featureFlagFork\n  featureFlagFryer\n  featureFlagGaplessFTDC\n  featureFlagInDevelopmentForTest\n  featureFlagIngressRateLimiting\n  featureFlagIntentRegistration\n  featureFlagInternalWritesAreReplicatedTransactionally\n  featureFlagLargeBatchedOperations\n  featureFlagLookupEncryptionSchemasFLE\n  featureFlagMandatoryAuthzChecks\n  featureFlagMongodProxyProtocolSupport\n  featureFlagMongotIndexedViews\n  featureFlagMoveCollection\n  featureFlagMqlJsEngineGap\n  featureFlagMultiPlanLimiter\n  featureFlagMultiServiceLogAndFTDCFormat\n  featureFlagOplogVisibility\n  featureFlagPQSBackfill\n  featureFlagPauseMigrationsDuringMultiUpdatesAvailable\n  featureFlagPersistRecipientPlacementInfoInMigrationRecoveryDoc\n  featureFlagPrefetch\n  featureFlagPrimaryDrivenIndexBuilds\n  featureFlagQETextSearchPreview\n  featureFlagQueryMemoryTracking\n  featureFlagQuerySettings\n  featureFlagQueryStats\n  featureFlagQueryStatsCountDistinct\n  featureFlagRankFusionBasic\n  featureFlagRankFusionFull\n  featureFlagRateLimitIngressConnectionEstablishment\n  featureFlagRawDataCrudOperations\n  featureFlagRearchitectUserAcquisition\n  featureFlagRecordDelinquentMetrics\n  featureFlagRecordIdsReplicated\n  featureFlagReduceMajorityWriteLatency\n  featureFlagRelaxedMode\n  featureFlagReleasedForTest\n  featureFlagRemoveLegacyTimeseriesBucketingParametersHaveChanged\n  featureFlagReplicaSetEndpoint\n  featureFlagReplicateLocalCatalogIdentifiers\n  featureFlagRequireTenantID\n  featureFlagReshardingAbortUnpreparedTransactionsUponPreparingToBlockWrites\n  featureFlagReshardingCloneNoRefresh\n  featureFlagReshardingForTimeseries\n  featureFlagReshardingNumSamplesPerChunk\n  featureFlagReshardingRemainingTimeEstimateBasedOnMovingAverage\n  featureFlagReshardingSkipCloningAndApplyingIfApplicable\n  featureFlagReshardingStoreOplogFetcherProgress\n  featureFlagReshardingVerification\n  featureFlagRouterPort\n  featureFlagSbeBlockHashAgg\n  featureFlagSbeFull\n  featureFlagSbeUpgradeBinaryTrees\n  featureFlagSearchBatchSizeTuning\n  featureFlagSearchExplainExecutionStats\n  featureFlagSearchHybridScoringFull\n  featureFlagSearchInSbe\n  featureFlagSecondaryIndexChecksInDbCheck\n  featureFlagSecurityToken\n  featureFlagServerlessChangeStreams\n  featureFlagSessionsCollectionCoordinatorOnConfigServer\n  featureFlagSetFcvDryRunMode\n  featureFlagShardAuthoritativeCollMetadata\n  featureFlagShardAuthoritativeDbMetadataCRUD\n  featureFlagShardAuthoritativeDbMetadataDDL\n  featureFlagShardFilteringDistinctScan\n  featureFlagShardedAggregationCatalogCacheGossiping\n  featureFlagSnapshotFCVInDDLCoordinators\n  featureFlagSpoon\n  featureFlagStorageEngineInterruptibility\n  featureFlagStreams\n  featureFlagTSBucketingParametersUnchanged\n  featureFlagTargetedMirrorReads\n  featureFlagTerminateSecondaryReadsUponRangeDeletion\n  featureFlagTimeSeriesInSbe\n  featureFlagTimeseriesDeletesSupport\n  featureFlagTimeseriesUpdatesSupport\n  featureFlagToaster\n  featureFlagTrackUnshardedCollectionsUponMoveCollection\n  featureFlagTransitionToCatalogShard\n  featureFlagUUIDExpression\n  featureFlagUnshardCollection\n  featureFlagUpdateDocumentShardKeyUsingTransactionApi\n  featureFlagUpgradingToDowngrading\n  featureFlagUseTopologyChangeCoordinators\n  featureFlagUseTwoStepRSToShardPromotion\n  featureFlagUseUnreplicatedTruncatesForDeletions\n  featureFlagVectorSearchPublicPreview\n  featureFlagVectorSimilarityExpressions\n  fileBasedInitialSyncExtendCursorTimeoutMS\n  fileBasedInitialSyncMaxCyclesWithoutProgress\n  fileBasedInitialSyncMaxLagSec\n  findChunksOnConfigTimeoutMS\n  flowControlDecayConstant\n  flowControlFudgeFactor\n  flowControlMaxSamples\n  flowControlMinTicketsPerSecond\n  flowControlSamplePeriod\n  flowControlTargetLagSeconds\n  flowControlThresholdLagPercentage\n  flowControlTicketAdderConstant\n  flowControlTicketMultiplierConstant\n  flowControlWarnThresholdSeconds\n  globalConnPoolIdleTimeoutMinutes\n  heartBeatFrequencyMs\n  honorSystemUmask\n  httpVerboseLogging\n  indexBuildMinAvailableDiskSpaceMB\n  indexMaxNumGeneratedKeysPerDocument\n  ingressAdmissionControlEnabled\n  ingressAdmissionControllerMaxQueueDepth\n  ingressAdmissionControllerTicketPoolSize\n  ingressConnectionEstablishmentBurstCapacitySecs\n  ingressConnectionEstablishmentMaxQueueDepth\n  ingressConnectionEstablishmentRateLimiterBypass\n  ingressConnectionEstablishmentRateLimiterEnabled\n  ingressConnectionEstablishmentRatePerSec\n  ingressRequestAdmissionBurstCapacitySecs\n  ingressRequestAdmissionRatePerSec\n  ingressRequestRateLimiterApplicationExemptions\n  ingressRequestRateLimiterEnabled\n  ingressRequestRateLimiterExemptions\n  initialSyncIndexBuildMemoryMaxMB\n  initialSyncIndexBuildMemoryMinMB\n  initialSyncIndexBuildMemoryPercentage\n  initialSyncMethod\n  initialSyncOplogBuffer\n  initialSyncOplogBufferPeekCacheSize\n  initialSyncOplogFetcherBatchSize\n  initialSyncSourceReadPreference\n  initialSyncTransientErrorRetryPeriodSeconds\n  internalChangeStreamUseTenantIdForTesting\n  internalDocumentSourceBucketAutoMaxMemoryBytes\n  internalDocumentSourceCursorBatchSizeBytes\n  internalDocumentSourceCursorInitialBatchSize\n  internalDocumentSourceDensifyMaxMemoryBytes\n  internalDocumentSourceGraphLookupMaxMemoryBytes\n  internalDocumentSourceGroupMaxMemoryBytes\n  internalDocumentSourceLookupCacheSizeBytes\n  internalDocumentSourceSetWindowFieldsMaxMemoryBytes\n  internalGeoNearQuery2DMaxCoveringCells\n  internalGeoPredicateQuery2DMaxCoveringCells\n  internalGraphLookupStageIntermediateDocumentMaxSizeBytes\n  internalIndexBuildBulkLoadYieldIterations\n  internalInsertMaxBatchSize\n  internalLookupStageIntermediateDocumentMaxSizeBytes\n  internalMaxSubPipelineViewDepth\n  internalMeasureQueryExecutionTimeInNanoseconds\n  internalPartialFilterExpressionMaxDepth\n  internalPipelineLengthLimit\n  internalProfilingMaxAbandonedWritesPerSecondPerDb\n  internalProhibitShardOperationRetry\n  internalQueryAggMulticastMaxConcurrency\n  internalQueryAggMulticastTimeoutMS\n  internalQueryAlwaysMergeOnPrimaryShard\n  internalQueryAppendIdToSetWindowFieldsSort\n  internalQueryAutoParameterizationMaxParameterCount\n  internalQueryBitTestPositionsLogThreshold\n  internalQueryCacheDisableInactiveEntries\n  internalQueryCacheEvictionRatio\n  internalQueryCacheMaxEntriesPerCollection\n  internalQueryCacheMaxSizeBytesBeforeStripDebugInfo\n  internalQueryCacheSize\n  internalQueryCacheWorksGrowthCoefficient\n  internalQueryCollectOptimizerMetrics\n  internalQueryCollectionMaxDataSizeBytesToChooseHashJoin\n  internalQueryCollectionMaxNoOfDocumentsToChooseHashJoin\n  internalQueryCollectionMaxStorageSizeBytesToChooseHashJoin\n  internalQueryConcurrentMultiPlanningThreshold\n  internalQueryDefaultDOP\n  internalQueryDesugarWhereToFunction\n  internalQueryDisableExchange\n  internalQueryDisableExclusionProjectionFastPath\n  internalQueryDisableLookupExecutionUsingHashJoin\n  internalQueryDisablePlanCache\n  internalQueryDisableSingleFieldExpressExecutor\n  internalQueryDoNotOpenContainedOrsInSimplifier\n  internalQueryDocumentSourceWriterBatchExtraReservedBytes\n  internalQueryEnableAggressiveSpillsInGroup\n  internalQueryEnableBooleanExpressionsSimplifier\n  internalQueryEnableCSTParser\n  internalQueryEnumerationMaxIntersectPerAnd\n  internalQueryEnumerationMaxOrSolutions\n  internalQueryEnumerationPreferLockstepOrEnumeration\n  internalQueryExecYieldIterations\n  internalQueryExecYieldPeriodMS\n  internalQueryExplainSizeThresholdBytes\n  internalQueryExpressionInterruptIterations\n  internalQueryExpressionInterruptPeriodMS\n  internalQueryExtraPredicateForReversedIn\n  internalQueryFLEAlwaysUseEncryptedCollScanMode\n  internalQueryFLERewriteMemoryLimit\n  internalQueryFacetBufferSizeBytes\n  internalQueryFacetMaxOutputDocSizeBytes\n  internalQueryFindCommandBatchSize\n  internalQueryForceIntersectionPlans\n  internalQueryFrameworkControl\n  internalQueryGetMoreMaxCursorPinRetryAttempts\n  internalQueryGlobalProfilingFilter\n  internalQueryGlobalProfilingLockDeadlineMs\n  internalQueryIgnoreUnknownJSONSchemaKeywords\n  internalQueryJavaScriptFnTimeoutMillis\n  internalQueryJavaScriptHeapSizeLimitMB\n  internalQueryMaxAddToSetBytes\n  internalQueryMaxAllowedDensifyDocs\n  internalQueryMaxBitTestIntermediatePositions\n  internalQueryMaxBlockingSortMemoryUsageBytes\n  internalQueryMaxConcatArraysBytes\n  internalQueryMaxConcurrentMultiPlanJobsPerCacheKey\n  internalQueryMaxDocValidationErrorConsideredValues\n  internalQueryMaxJsEmitBytes\n  internalQueryMaxMapReduceExpressionBytes\n  internalQueryMaxNumPrimeImplicants\n  internalQueryMaxPercentileAccumulatorBytes\n  internalQueryMaxPushBytes\n  internalQueryMaxRangeBytes\n  internalQueryMaxScansToExplode\n  internalQueryMaxSetUnionBytes\n  internalQueryMaxSizeFactorToSimplify\n  internalQueryMaxSpoolDiskUsageBytes\n  internalQueryMaxSpoolMemoryUsageBytes\n  internalQueryMaximumNumberOfMintermsInSimplifier\n  internalQueryMaximumNumberOfUniquePredicatesToSimplify\n  internalQueryNumChunksForChunkBasedSampling\n  internalQueryPercentileExprSelectToSortThreshold\n  internalQueryPermitMatchSwappingForComplexRenames\n  internalQueryPlanEvaluationCollFraction\n  internalQueryPlanEvaluationMaxResults\n  internalQueryPlanEvaluationWorks\n  internalQueryPlanOrChildrenIndependently\n  internalQueryPlanTieBreakingWithIndexHeuristics\n  internalQueryPlanTotalEvaluationCollFraction\n  internalQueryPlannerEnableHashIntersection\n  internalQueryPlannerEnableIndexIntersection\n  internalQueryPlannerEnableIndexPruning\n  internalQueryPlannerGenerateCoveredWholeIndexScans\n  internalQueryPlannerIgnoreIndexWithCollationForRegex\n  internalQueryPlannerMaxIndexedSolutions\n  internalQueryPlannerUseMultiplannerForSingleSolutions\n  internalQueryProhibitBlockingMergeOnMongoS\n  internalQueryProhibitMergingOnMongoS\n  internalQueryRegexHeapLimitKB\n  internalQueryRegexMatchLimit\n  internalQueryS2GeoCoarsestLevel\n  internalQueryS2GeoFinestLevel\n  internalQueryS2GeoMaxCells\n  internalQuerySBEPlanEvaluationMaxMemoryBytes\n  internalQuerySamplingBySequentialScan\n  internalQuerySamplingCEMethod\n  internalQuerySettingsBackfillDelaySeconds\n  internalQuerySettingsBackfillMemoryLimitBytes\n  internalQuerySlotBasedExecutionDisableGroupPushdown\n  internalQuerySlotBasedExecutionDisableLookupPushdown\n  internalQuerySlotBasedExecutionDisableTimeSeriesPushdown\n  internalQuerySlotBasedExecutionHashAggApproxMemoryUseInBytesBeforeSpill\n  internalQuerySlotBasedExecutionHashAggIncreasedSpilling\n  internalQuerySlotBasedExecutionHashAggMemoryCheckPerAdvanceAtLeast\n  internalQuerySlotBasedExecutionHashAggMemoryCheckPerAdvanceAtMost\n  internalQuerySlotBasedExecutionHashAggMemoryUseCheckMargin\n  internalQuerySlotBasedExecutionHashLookupApproxMemoryUseInBytesBeforeSpill\n  internalQuerySlotBasedExecutionMaxStaticIndexScanIntervals\n  internalQuerySlotBasedExecutionWindowBufferMemorySamplingAtLeast\n  internalQuerySlotBasedExecutionWindowStateMemorySamplingAtLeast\n  internalQuerySpillingMaxWaitTimeout\n  internalQuerySpillingMinAvailableDiskSpaceBytes\n  internalQueryStatsCacheSize\n  internalQueryStatsErrorsAreCommandFatal\n  internalQueryStatsRateLimit\n  internalQueryStatsSampleRate\n  internalQueryTdigestDelta\n  internalQueryTopNAccumulatorBytes\n  internalQueryUnifiedWriteExecutor\n  internalReduceAccumulatedValueDepthCheckInterval\n  internalRoaringBitmapsBatchSize\n  internalRoaringBitmapsMinimalDensity\n  internalRoaringBitmapsThreshold\n  internalSessionsReapThreshold\n  internalTextOrStageMaxMemoryBytes\n  internalValidateFeaturesAsMaster\n  internalValidateFeaturesAsPrimary\n  javascriptProtection\n  joinIngressSessionsOnShutdown\n  journalCommitInterval\n  jsHeapLimitMB\n  loadBalancerPort\n  loadRoutingTableOnStartup\n  localLogicalSessionTimeoutMinutes\n  localThresholdMs\n  logComponentVerbosity\n  logLevel\n  logicalSessionRefreshMillis\n  maxAcceptableLogicalClockDriftSecs\n  maxBSONDepth\n  maxCatchUpPercentageBeforeBlockingWrites\n  maxDbCheckMBperSec\n  maxIncomingConnectionsOverride\n  maxIndexBuildDrainBatchSize\n  maxIndexBuildDrainMemoryUsageMegabytes\n  maxIndexBuildMemoryUsageMegabytes\n  maxIteratorsMemoryUsagePercentage\n  maxLogSizeKB\n  maxNumActiveUserIndexBuilds\n  maxNumStaleVersionRetries\n  maxNumSyncSourceChangesPerHour\n  maxNumberOfBatchedOperationsInSingleOplogEntry\n  maxNumberOfInsertsBatchInsertsForRenameAcrossDatabases\n  maxNumberOfTransactionOperationsInSingleOplogEntry\n  maxOplogTruncationPointsAfterStartup\n  maxOplogTruncationPointsDuringStartup\n  maxRoundsWithoutProgressParameter\n  maxSessions\n  maxSizeOfBatchedInsertsForRenameAcrossDatabasesBytes\n  maxSizeOfBatchedOperationsInSingleOplogEntryBytes\n  maxSyncSourceLagSecs\n  maxTimeMSForHedgedReads\n  maxTransactionLockRequestTimeoutMillis\n  maxValidateMBperSec\n  maxValidateMemoryUsageMB\n  messageSizeErrorRateSec\n  metadataRefreshInTransactionMaxWaitBehindCritSecMS\n  metadataRefreshInTransactionMaxWaitMS\n  migrateCloneInsertionBatchDelayMS\n  migrateCloneInsertionBatchSize\n  migrationLockAcquisitionMaxWaitMS\n  minNumChunksForSessionsCollection\n  minOplogTruncationPoints\n  minSnapshotHistoryWindowInSeconds\n  minimalWriteConflictRetryCountForStateDump\n  mirrorReads\n  mirrorReadsMaxConnPoolSize\n  mongosShutdownTimeoutMillisForSignaledShutdown\n  mongotConnectionPoolMaxSize\n  mongotConnectionPoolMinSize\n  mongotHost\n  mrEnableSingleReduceOptimization\n  multiUpdateCoordinatorServiceMaxThreadCount\n  multiUpdateCoordinatorServiceMinThreadCount\n  multitenancySupport\n  newShardExistingClusterTimeKeysExpirationSecs\n  notablescan\n  numInitialSyncAttempts\n  numInitialSyncConnectAttempts\n  numInitialSyncOplogFindAttempts\n  ocspCacheSize\n  ocspEnabled\n  ocspStaplingRefreshPeriodSecs\n  ocspValidationRefreshPeriodSecs\n  openTelemetryMetricsDirectory\n  openTelemetryMetricsHttpEndpoint\n  opensslCipherConfig\n  opensslCipherSuiteConfig\n  opensslDiffieHellmanParameters\n  operationMemoryPoolBlockInitialSizeKB\n  operationMemoryPoolBlockMaxSizeKB\n  oplogApplicationEnforcesSteadyStateConstraints\n  oplogBatchDelayMillis\n  oplogFetcherInitialSyncMaxFetcherRestarts\n  oplogFetcherSteadyStateMaxFetcherRestarts\n  oplogFetcherUsesExhaust\n  oplogInitialFindMaxSeconds\n  oplogNetworkTimeoutBufferSeconds\n  oplogRetriedFindMaxSeconds\n  oplogSamplingAsyncEnabled\n  oplogSamplingAsyncYieldIntervalMs\n  oplogSamplingLogIntervalSeconds\n  oplogTruncationCheckPeriodSeconds\n  oplogTruncationPointSizeMB\n  opportunisticSecondaryTargeting\n  orphanCleanupDelaySecs\n  overdueInterruptCheckIntervalMillis\n  performTimeseriesCompressionIntermediateDataIntegrityCheckOnInsert\n  performTimeseriesCompressionIntermediateDataIntegrityCheckOnReopening\n  periodicNoopIntervalSecs\n  persistedChunkCacheUpdateMaxBatchSize\n  pessimisticConnectivityCheckForAcceptedConnections\n  pinTaskExecCursorConns\n  planCacheSize\n  planRankerMode\n  preAuthMaximumMessageSizeBytes\n  preImagesCollectionTruncateMarkersMinBytes\n  priorityTakeoverFreshnessWindowSeconds\n  processUmask\n  proxyProtocolMaximumPendingConnections\n  proxyProtocolMaximumWaitBackoffMillis\n  proxyProtocolTimeoutSecs\n  queryAnalysisQueryStatsSmoothingFactor\n  queryAnalysisSampleExpirationSecs\n  queryAnalysisSamplerBurstMultiplier\n  queryAnalysisSamplerConfigurationRefreshSecs\n  queryAnalysisSamplerInActiveThresholdSecs\n  queryAnalysisWriterIntervalSecs\n  queryAnalysisWriterMaxBatchSize\n  queryAnalysisWriterMaxMemoryUsageBytes\n  queryAnalysisWriterMaxThreadPoolSize\n  queryAnalysisWriterMinThreadPoolSize\n  quiet\n  rangeDeleterBatchDelayMS\n  rangeDeleterBatchSize\n  readHedgingMode\n  receiveChunkWaitForRangeDeleterTimeoutMS\n  recoverFromOplogAsStandalone\n  recoverToOplogTimestamp\n  redactEncryptedFields\n  remainingReshardingOperationTimePrepareThresholdMillis\n  remainingReshardingOperationTimeThresholdMillis\n  replBatchLimitBytes\n  replBatchLimitOperations\n  replElectionTimeoutOffsetLimitFraction\n  replWriterMinThreadCount\n  replWriterThreadCount\n  replicaSetMonitorProtocol\n  reportOpWriteConcernCountersInServerStatus\n  requireApiVersion\n  reshardingAbortUnpreparedTransactionsUponPreparingToBlockWrites\n  reshardingCollectionClonerBatchSizeCount\n  reshardingCollectionClonerMaxStalenessSeconds\n  reshardingCollectionClonerWriteThreadCount\n  reshardingCoordinatorQuiescePeriodMillis\n  reshardingCoordinatorServiceMaxThreadCount\n  reshardingCriticalSectionTimeoutMillis\n  reshardingDelayBeforeRemainingOperationTimeQueryMillis\n  reshardingDonorServiceMaxThreadCount\n  reshardingExponentialMovingAverageTimeToFetchAndApplyIntervalMillis\n  reshardingExponentialMovingAverageTimeToFetchAndApplySmoothingFactor\n  reshardingMaxDelayBetweenRemainingOperationTimeQueriesMillis\n  reshardingMaxReplicationLagSecondsBeforeBuildingIndexes\n  reshardingMinimumOperationDurationMillis\n  reshardingOplogApplierMaxLockRequestTimeoutMillis\n  reshardingOplogBatchLimitBytes\n  reshardingOplogBatchLimitOperations\n  reshardingOplogBatchTaskCount\n  reshardingOplogFetcherInsertBatchLimitBytes\n  reshardingOplogFetcherInsertBatchLimitOperations\n  reshardingOplogFetcherSleepMillisBeforeCriticalSection\n  reshardingOplogFetcherSleepMillisDuringCriticalSection\n  reshardingOplogFetcherTargetPrimaryDuringCriticalSection\n  reshardingRecipientServiceMaxThreadCount\n  reshardingRemainingTimeEstimateAccountsForDonorReplicationLag\n  reshardingRemainingTimeEstimateAccountsForRecipientReplicationLag\n  reshardingRemainingTimeEstimateBasedOnMovingAverage\n  reshardingTxnClonerProgressBatchSize\n  reshardingVerificationChangeStreamsEventsBatchSizeLimit\n  reshardingVerificationChangeStreamsEventsBatchTimeLimitSeconds\n  reshardingVerificationChangeStreamsSleepMS\n  reshardingWaitForReplicationIntervalMilliseconds\n  reshardingWaitForReplicationTimeoutSeconds\n  resumableIndexBuildMajorityOpTimeTimeoutMillis\n  rewriteStateChangeErrors\n  roleGraphInvalidationIsFatal\n  rollbackRemoteOplogQueryBatchSize\n  rollbackTimeLimitSecs\n  routingTableCacheChunkBucketSize\n  samplingConfidenceInterval\n  samplingMarginOfError\n  saslHostName\n  saslServiceName\n  saslauthdPath\n  scramIterationCount\n  scramSHA256IterationCount\n  scriptingEngineInterruptIntervalMS\n  searchIndexManagementHostAndPort\n  searchTLSMode\n  sessionMaxBatchSize\n  sessionWriteConcernTimeoutSystemMillis\n  shardedIndexConsistencyCheckIntervalMS\n  shutdownTimeoutMillisForSignaledShutdown\n  skipApplyingDbCheckBatchOnSecondary\n  skipAuthenticationToMongot\n  skipAuthenticationToSearchIndexManagementServer\n  skipCorruptDocumentsWhenCloning\n  skipDroppingHashedShardKeyIndex\n  skipOplogSampling\n  skipShardingConfigurationChecks\n  skipShellCursorFinalize\n  slowConnectionThresholdMillis\n  slowServiceOnStepDownThresholdMS\n  slowServiceOnStepUpBeginThresholdMS\n  slowServiceOnStepUpCompleteThresholdMS\n  slowTotalOnStepUpBeginThresholdMS\n  slowTotalOnStepUpCompleteThresholdMS\n  spTestNeedsFeatureFlagBlender\n  spTestNeedsFeatureFlagToaster\n  spTestNeedsLatestFCV\n  spillTableInsertBatchSizeBytes\n  spillWiredTigerBlockCompressor\n  spillWiredTigerCacheSizeMaxMB\n  spillWiredTigerCacheSizeMinMB\n  spillWiredTigerCacheSizePercentage\n  spillWiredTigerEngineConfig\n  spillWiredTigerEngineRuntimeConfig\n  spillWiredTigerEvictionDirtyTargetPercentage\n  spillWiredTigerEvictionDirtyTriggerPercentage\n  spillWiredTigerEvictionThreadsMax\n  spillWiredTigerEvictionThreadsMin\n  spillWiredTigerEvictionUpdatesTriggerPercentage\n  spillWiredTigerServerStatusVerbosity\n  spillWiredTigerSessionMax\n  spillWiredTigerZstdCompressionLevel\n  sslMode\n  sslWithholdClientCertificate\n  startupAuthSchemaValidation\n  startupRecoveryForRestore\n  storageEngineConcurrencyAdjustmentAlgorithm\n  storageEngineConcurrencyAdjustmentIntervalMillis\n  storageEngineConcurrentReadTransactions\n  storageEngineConcurrentWriteTransactions\n  storageEngineReadMaxQueueDepth\n  storageEngineWriteMaxQueueDepth\n  storageGlobalParams.directoryperdb\n  supportArbitraryClusterKeyIndex\n  suppressNoTLSPeerCertificateWarning\n  syncdelay\n  takeUnstableCheckpointOnShutdown\n  taskExecutorPoolSize\n  tcpFastOpenClient\n  tcpFastOpenQueueSize\n  tcpFastOpenServer\n  temporarilyUnavailableBackoffBaseMs\n  temporarilyUnavailableMaxRetries\n  terminateSecondaryReadsOnOrphanCleanup\n  testMongosOnlyRuntimeParameter\n  testOnlyValidatedTenancyScopeKey\n  testingDiagnosticsEnabled\n  testingSnapshotBehaviorInIsolation\n  throughputProbingConcurrencyMovingAverageWeight\n  throughputProbingInitialConcurrency\n  throughputProbingMaxConcurrency\n  throughputProbingMinConcurrency\n  throughputProbingReadWriteRatio\n  throughputProbingStallDetectionTimeoutMs\n  throughputProbingStepMultiple\n  timeseriesBucketMaxCount\n  timeseriesBucketMaxSize\n  timeseriesBucketMinCount\n  timeseriesBucketMinSize\n  timeseriesIdleBucketExpiryMaxCountPerAttempt\n  timeseriesIdleBucketExpiryMemoryUsageThreshold\n  timeseriesInsertMaxRetriesOnDuplicates\n  timeseriesLargeMeasurementThreshold\n  timeseriesMaxRetriesForWriteConflictsOnReopening\n  timeseriesSideBucketCatalogMemoryUsageThreshold\n  timeseriesValidateCompressedBuckets\n  tlsClusterAuthX509Override\n  tlsMode\n  tlsOCSPSlowResponderWarningSecs\n  tlsOCSPStaplingTimeoutSecs\n  tlsOCSPVerifyTimeoutSecs\n  tlsUseSystemCA\n  tlsWithholdClientCertificate\n  tlsX509ClusterAuthDNOverride\n  tlsX509ExpirationWarningThresholdDays\n  traceExceptions\n  traceWriteConflictExceptions\n  trafficRecordingDirectory\n  transactionLifetimeLimitSeconds\n  transactionSizeLimitBytes\n  transactionTooLargeForCacheThreshold\n  ttlIndexDeleteTargetDocs\n  ttlIndexDeleteTargetTimeMS\n  ttlMonitorBatchDeletes\n  ttlMonitorEnabled\n  ttlMonitorMaxMetadataRecoveryThreads\n  ttlMonitorSleepSecs\n  ttlMonitorSubPassTargetSecs\n  unsupportedDangerousTestingFLEDiagnosticsEnabled\n  unsupportedSyncSource\n  upsertMaxRetryAttemptsOnDuplicateKeyError\n  useBatchedDeletesForRangeDeletion\n  useGrpcForSearch\n  useReadOnceCursorsForIndexBuilds\n  useSlowCollectionTruncateMarkerScanning\n  userCacheInvalidationIntervalSecs\n  validateCollectionName\n  validateDbName\n  waitForSecondaryBeforeNoopWriteMS\n  warmMinConnectionsInShardingTaskExecutorPoolOnStartup\n  warmMinConnectionsInShardingTaskExecutorPoolOnStartupWaitMS\n  watchdogPeriodSeconds\n  wiredTigerCheckpointCleanupPeriodSeconds\n  wiredTigerConcurrentReadTransactions\n  wiredTigerConcurrentWriteTransactions\n  wiredTigerCursorCacheSize\n  wiredTigerDirectoryForIndexes\n  wiredTigerEngineRuntimeConfig\n  wiredTigerEvictionDirtyMaxGB\n  wiredTigerEvictionDirtyTargetGB\n  wiredTigerEvictionUpdatesMaxGB\n  wiredTigerExtraDiagnostics\n  wiredTigerFileHandleCloseIdleTime\n  wiredTigerFileHandleCloseMinimum\n  wiredTigerFileHandleCloseScanInterval\n  wiredTigerSessionCacheMaxPercentage\n  wiredTigerSessionCloseIdleTimeSecs\n  wiredTigerSessionMax\n  wiredTigerSizeStorerPeriodicSyncHits\n  wiredTigerSizeStorerPeriodicSyncPeriodMillis\n  wiredTigerSkipTableLoggingChecksDuringValidation\n  wiredTigerSkipTableLoggingChecksOnStartup\n  wiredTigerStressConfig\n  wiredTigerVerboseShutdownCheckpointLogs\n  writeConflictRetryCountForDumpState\n  writeConflictRetryLimit\n  writePeriodicNoops\n{ getParameter:'*' } or { getParameter:{allParameters: true} } to get everything\n",
    ),
    (
        "getQueryableEncryptionCountInfo",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "getShardMap",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "lists the set of shards known to this instance",
    ),
    (
        "getShardVersion",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        " example: { getShardVersion : 'alleyinsider.foo'  } ",
    ),
    (
        "getTrafficRecordingStatus",
        SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "grantPrivilegesToRole",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "grantRolesToRole",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "grantRolesToUser",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "hello",
        SECONDARY_OK | API_V1,
        "Check if this server is primary for a replica set\n{ hello : 1 }",
    ),
    (
        "hostInfo",
        SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "insert",
        REQUIRES_AUTH | API_V1,
        "insert documents",
    ),
    (
        "internalRenameIfOptionsAndIndexesMatch",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Internal command to rename and check collection options",
    ),
    (
        "invalidateUserCache",
        SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "isMaster",
        SECONDARY_OK,
        "Check if this server is primary for a replica set\n{ isMaster : 1 }",
    ),
    (
        "killAllSessions",
        SECONDARY_OK | REQUIRES_AUTH,
        "kill all logical sessions, for a user, and their operations",
    ),
    (
        "killAllSessionsByPattern",
        SECONDARY_OK | REQUIRES_AUTH,
        "kill logical sessions by pattern",
    ),
    (
        "killCursors",
        SECONDARY_OK | REQUIRES_AUTH | API_V1,
        "Kill a list of cursor ids",
    ),
    (
        "killOp",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "killSessions",
        SECONDARY_OK | REQUIRES_AUTH,
        "kill a logical session and its operations",
    ),
    (
        "listCollections",
        REQUIRES_AUTH | API_V1 | SECONDARY_OVERRIDE_OK,
        "list collections for this db",
    ),
    (
        "listCommands",
        SECONDARY_OK,
        "get a list of all db commands",
    ),
    (
        "listDatabases",
        ADMIN_ONLY | REQUIRES_AUTH | API_V1 | SECONDARY_OVERRIDE_OK,
        "{ listDatabases:1, [filter: <filterObject>] [, nameOnly: true ] }\nlist databases on this server",
    ),
    (
        "listDatabasesForAllTenants",
        ADMIN_ONLY | REQUIRES_AUTH | SECONDARY_OVERRIDE_OK,
        "{ listDatabasesForAllTenants:1, [filter: <filterObject>] [, nameOnly: true ] }\ncommand which lists databases for all tenants on this server",
    ),
    (
        "listIndexes",
        REQUIRES_AUTH | API_V1 | SECONDARY_OVERRIDE_OK,
        "list indexes for a collection",
    ),
    (
        "listSearchIndexes",
        REQUIRES_AUTH,
        "Command to list search indexes. Only supported with Atlas.",
    ),
    (
        "lockInfo",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "show all lock info on the server",
    ),
    (
        "logRotate",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "logout",
        SECONDARY_OK | REQUIRES_AUTH,
        "de-authenticate",
    ),
    (
        "mapReduce",
        REQUIRES_AUTH | SECONDARY_OVERRIDE_OK,
        "Runs the mapReduce command. See http://dochub.mongodb.org/core/mapreduce for details.",
    ),
    (
        "mergeChunks",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Internal command to merge a contiguous range of chunks.\nUsage: { _shardsvrMergeChunks: <ns>, epoch: <epoch>, bounds: [<min key>, <max key>] }",
    ),
    (
        "ping",
        SECONDARY_OK | API_V1,
        "a way to check that the server is alive. responds immediately even if server is in a db lock.",
    ),
    (
        "planCacheClear",
        REQUIRES_AUTH | SECONDARY_OVERRIDE_OK,
        "Drops one or all plan cache entries in a collection.",
    ),
    (
        "planCacheClearFilters",
        REQUIRES_AUTH | SECONDARY_OVERRIDE_OK,
        "Clears index filter for a single query shape or, if the query shape is omitted, all filters for the collection.",
    ),
    (
        "planCacheListFilters",
        REQUIRES_AUTH | SECONDARY_OVERRIDE_OK,
        "Displays index filters for all query shapes in a collection.",
    ),
    (
        "planCacheSetFilter",
        REQUIRES_AUTH | SECONDARY_OVERRIDE_OK,
        "Sets index filter for a query shape. Overrides existing filter.",
    ),
    (
        "prepareTransaction",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Prepares a transaction on this shard; sent by a router or re-sent by the transaction commit coordinator for a cross-shard transaction",
    ),
    (
        "profile",
        SECONDARY_OK | REQUIRES_AUTH,
        "controls the behaviour of the performance profiler, the fraction of eligible operations which are sampled for logging/profiling, and the threshold duration at which ops become eligible. See http://docs.mongodb.org/manual/reference/command/profile",
    ),
    (
        "reIndex",
        SECONDARY_OK | REQUIRES_AUTH,
        "re-index a collection (can only be run on a standalone mongod)",
    ),
    (
        "refreshSessions",
        SECONDARY_OK | REQUIRES_AUTH | API_V1,
        "renew a set of logical sessions",
    ),
    (
        "releaseMemory",
        SECONDARY_OK | REQUIRES_AUTH,
        "ask given cursors to release memory if possible",
    ),
    (
        "removeQuerySettings",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Removes the query settings for the query shape of a given query.",
    ),
    (
        "renameCollection",
        ADMIN_ONLY | REQUIRES_AUTH | API_V1,
        " example: { renameCollection: foo.a, to: bar.b }",
    ),
    (
        "replSetAbortPrimaryCatchUp",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "{ CmdReplSetAbortPrimaryCatchUp : 1 }\nAbort primary catch-up mode; immediately finish the transition to primary without fetching any further unreplicated writes from any other online nodes",
    ),
    (
        "replSetFreeze",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "{ replSetFreeze : <seconds> }\n'freeze' state of member to the extent we can do that.  What this really means is that\nthis node will not attempt to become primary until the time period specified expires.\nYou can call again with {replSetFreeze:0} to unfreeze sooner.\nA process restart unfreezes the member also.\nhttp://dochub.mongodb.org/core/replicasetcommands",
    ),
    (
        "replSetGetConfig",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Returns the current replica set configuration{ replSetGetConfig : 1 }\nhttp://dochub.mongodb.org/core/replicasetcommands",
    ),
    (
        "replSetGetRBID",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "replSetGetStatus",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Report status of a replica set from the POV of this server\n{ replSetGetStatus : 1 }\nhttp://dochub.mongodb.org/core/replicasetcommands",
    ),
    (
        "replSetHeartbeat",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "replSetInitiate",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Initiate/christen a replica set.\nhttp://dochub.mongodb.org/core/replicasetcommands",
    ),
    (
        "replSetMaintenance",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "{ replSetMaintenance : bool }\nEnable or disable maintenance mode.",
    ),
    (
        "replSetReconfig",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Adjust configuration of a replica set\n{ replSetReconfig : config_object }\nhttp://dochub.mongodb.org/core/replicasetcommands",
    ),
    (
        "replSetRequestVotes",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "replSetResizeOplog",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Resize oplog using size (in MBs) and optionally, retention (in terms of hours)",
    ),
    (
        "replSetStepDown",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "{ replSetStepDown : <seconds> }\nStep down as primary.  Will not try to reelect self for the specified time period (1 minute if no numeric secs value specified, or secs is 0).\n(If another member with same priority takes over in the meantime, it will stay primary.)\nhttp://dochub.mongodb.org/core/replicasetcommands",
    ),
    (
        "replSetStepUp",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "replSetSyncFrom",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "{ replSetSyncFrom : \"host:port\" }\nChange who this member is syncing from. Note: This will interrupt and restart an in-progress initial sync.",
    ),
    (
        "replSetUpdatePosition",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "revokePrivilegesFromRole",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "revokeRolesFromRole",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "revokeRolesFromUser",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "rolesInfo",
        REQUIRES_AUTH | SECONDARY_OVERRIDE_OK,
        "no help defined",
    ),
    (
        "rotateCertificates",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Rotate certificates for new SSL connections",
    ),
    (
        "rotateFTDC",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "saslContinue",
        SECONDARY_OK | API_V1,
        "Subsequent steps in a SASL authentication conversation.",
    ),
    (
        "saslStart",
        SECONDARY_OK | API_V1,
        "First step in a SASL authentication conversation.",
    ),
    (
        "serverStatus",
        SECONDARY_OK | REQUIRES_AUTH,
        "returns lots of administrative server statistics. Optionally set {..., all: 1} to retrieve all server status sections.",
    ),
    (
        "setChangeStreamState",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Sets the change stream state in the serverless\nUsage:\n    {setChangeStreamState: 1, enabled: <true|false>}\nFields:\n    enabled:  enable or disable the change stream",
    ),
    (
        "setClusterParameter",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Set cluster parameter on replica set or node",
    ),
    (
        "setDefaultRWConcern",
        ADMIN_ONLY | REQUIRES_AUTH,
        "set the current read/write concern defaults (cluster-wide)",
    ),
    (
        "setFeatureCompatibilityVersion",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Set the featureCompatibilityVersion used by this cluster. If set to '8.0', then features introduced in versions greater than '8.0' will be disabled If set to '8.1', then features introduced in '8.2' will be disabled. If set to '8.2', then '8.2' features are enabled, and all nodes in the cluster must be binary version 8.2. See https://docs.mongodb.com/master/release-notes/8.0-compatibility/#feature-compatibility.",
    ),
    (
        "setIndexCommitQuorum",
        REQUIRES_AUTH,
        "Resets the commitQuorum for the given index builds in a collection. Usage:\n{\n    setIndexCommitQuorum: <string> collection name,\n    indexNames: array<string> list of index names,\n    commitQuorum: <string|number|object> option to define the required quorum for\n                  the index builds to commit\n}\nThis command is useful if the commitQuorum of an active index build is no longer possible or desirable (replica set membership has changed), or potential secondary replication lag has become a greater concern",
    ),
    (
        "setParameter",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "set administrative option(s)\n{ setParameter:1, <param>:<value> }\nsupported:\n  AbortExpiredTransactionsSessionCheckoutTimeout\n  AbortOldestTransactionMemoryClearLimitPerBatch\n  AbortOldestTransactionSessionCheckoutTimeoutMilliseconds\n  AbortOldestTransactionSessionKillLimitPerBatch\n  AlwaysRecordTraffic\n  KeysRotationIntervalSec\n  KillSessionsMaxConcurrency\n  KillSessionsPerHostTimeoutMS\n  ShardingTaskExecutorPoolHostTimeoutMS\n  ShardingTaskExecutorPoolMaxConnecting\n  ShardingTaskExecutorPoolMaxQueueDepth\n  ShardingTaskExecutorPoolMaxSize\n  ShardingTaskExecutorPoolMaxSizeForConfigServers\n  ShardingTaskExecutorPoolMinSize\n  ShardingTaskExecutorPoolMinSizeForConfigServers\n  ShardingTaskExecutorPoolRefreshRequirementMS\n  ShardingTaskExecutorPoolRefreshTimeoutMS\n  ShardingTaskExecutorPoolReplicaSetMatching\n  TransactionRecordMinimumLifetimeMinutes\n  acceptApiVersion2\n  allowDiskUseByDefault\n  allowDocumentsGreaterThanMaxUserSize\n  allowEmptyOplogBatchesToPropagateCommitPoint\n  allowMultipleArbiters\n  allowRolesFromX509Certificates\n  allowUnsafeUntimestampedWrites\n  analyzeShardKeyCharacteristicsDefaultSampleSize\n  analyzeShardKeyMonotonicityCorrelationCoefficientThreshold\n  analyzeShardKeyNumMostCommonValues\n  analyzeShardKeyNumRanges\n  analyzeShardKeyNumSamplesPerRange\n  analyzeShardKeySplitPointExpirationSecs\n  authSchemaVersion\n  authenticationMechanisms\n  authorizationManagerCacheSize\n  autoMergerIntervalSecs\n  autoMergerMaxTimeProcessingChunksMS\n  autoMergerThrottlingMS\n  automationServiceDescriptor\n  awsEC2InstanceMetadataUrl\n  awsECSInstanceMetadataUrl\n  backtraceLogFile\n  balancerChunksSelectionTimeoutMs\n  balancerMigrationsThrottlingMs\n  batchUserMultiDeletes\n  batchedDeletesTargetBatchDocs\n  batchedDeletesTargetBatchTimeMS\n  batchedDeletesTargetStagedDocBytes\n  bgSyncOplogFetcherBatchSize\n  bsonTestValidationVersion\n  buildInfoAuthMode\n  bulkWriteMaxRepliesSize\n  bypassRankFusionFCVGate\n  cachePressureEvictionStallDetectionWindowSeconds\n  cachePressureEvictionStallThresholdProportion\n  cachePressureExponentiallyDecayingMovingAverageAlphaValue\n  cachePressureQueryPeriodMilliseconds\n  capMemoryConsumptionForPreAuthBuffers\n  catalogCacheCollectionMaxEntries\n  catalogCacheDatabaseMaxEntries\n  changeCollectionExpiredDocumentsRemoverJobSleepSeconds\n  changeSyncSourceThresholdMillis\n  chunkDefragmentationThrottlingMS\n  chunkMigrationConcurrency\n  chunkMigrationFetcherMaxBufferedSizeBytesPerThread\n  clientCursorMonitorFrequencySecs\n  clientSourceAuthenticationRestrictionMode\n  clusterAuthMode\n  clusterIpSourceAllowlist\n  clusterServerParameterRefreshIntervalSecs\n  collectionBulkLoaderBatchSizeInBytes\n  collectionClonerBatchSize\n  collectionClonerUsesExhaust\n  collectionSamplingLogIntervalSeconds\n  connPoolMaxConnsPerHost\n  connPoolMaxInUseConnsPerHost\n  connectTimeoutMs\n  coordinateCommitReturnImmediatelyAfterPersistingDecision\n  createRollbackDataFiles\n  cursorTimeoutMillis\n  dbCheckHealthLogEveryNBatches\n  dbCheckMaxConsecutiveIdenticalIndexKeysPerSnapshot\n  dbCheckMaxInternalRetries\n  dbCheckMaxRunsOnQueue\n  dbCheckMaxTotalIndexKeysPerSnapshot\n  dbCheckSecondaryBatchMaxTimeMs\n  defaultConfigCommandTimeoutMS\n  defaultFindReplicaSetHostTimeoutMS\n  defaultStartupFCV\n  delinquentAcquisitionIntervalMillis\n  diagnosticDataCollectionDirectoryPath\n  diagnosticDataCollectionDirectorySizeMB\n  diagnosticDataCollectionDiscardLargeSamples\n  diagnosticDataCollectionEnableLatencyHistograms\n  diagnosticDataCollectionEnabled\n  diagnosticDataCollectionFileSizeMB\n  diagnosticDataCollectionMaxThreads\n  diagnosticDataCollectionMetadataCaptureFrequency\n  diagnosticDataCollectionMinThreads\n  diagnosticDataCollectionPeriodMillis\n  diagnosticDataCollectionSampleTimeoutMillis\n  diagnosticDataCollectionSamplesPerChunk\n  diagnosticDataCollectionSamplesPerInterimUpdate\n  diagnosticDataCollectionStatsNamespaces\n  diagnosticDataCollectionVerboseTCMalloc\n  directConnectionChecksWithSingleShard\n  disableExpiredChangeCollectionRemover\n  disableExpiredPreImagesRemover\n  disableLogicalSessionCacheRefresh\n  disableNonSSLConnectionLogging\n  disableNonTLSConnectionLogging\n  disableResumableRangeDeleter\n  disableSplitHorizonIPCheck\n  disableTransitionFromLatestToLastContinuous\n  disabledSecureAllocatorDomains\n  disaggregatedStorageEnabled\n  enableAccessToUserRoles\n  enableAutoCompaction\n  enableCompoundWildcardIndexLog\n  enableComputeMode\n  enableDefaultWriteConcernUpdatesForInitiate\n  enableDetailedConnectionHealthMetricLogLines\n  enableDiagnosticLogging\n  enableElectionHandoff\n  enableFinerGrainedCatalogCacheRefresh\n  enableFlowControl\n  enableIndexBuildCommitQuorum\n  enableLocalhostAuthBypass\n  enableOverrideClusterChainingSetting\n  enableQueryKilledByRangeDeletionLog\n  enableReconfigRollbackCommittedWritesCheck\n  enableSearchMeta\n  enableShardedIndexConsistencyCheck\n  enableTemporarilyUnavailableExceptions\n  enableTestCommands\n  enableTimeoutOfInactiveSessionCursors\n  enforceUserClusterSeparation\n  expiredChangeStreamPreImageRemovalJobSleepSecs\n  externalPipeDir\n  fassertOnLockTimeoutForStepUpDown\n  featureCompatibilityVersion\n  featureFlag80CollectionCreationPath\n  featureFlagAccuratePercentiles\n  featureFlagAggMongosToRouter\n  featureFlagAggOutTimeseries\n  featureFlagAllBinariesSupportRawDataOperations\n  featureFlagAllMongodsAreSharded\n  featureFlagAllowAdditionalParticipants\n  featureFlagAllowMergeOnNullishValues\n  featureFlagAlwaysCreateConfigTransactionsPartialIndexOnStepUp\n  featureFlagArrayAccumulators\n  featureFlagAutoCompact\n  featureFlagBalanceUnshardedCollections\n  featureFlagBalancerWindowDOW\n  featureFlagBanEncryptionOptionsInCollectionCreation\n  featureFlagBinDataConvert\n  featureFlagBinDataConvertNumeric\n  featureFlagBlender\n  featureFlagBulkWriteCommand\n  featureFlagChangeStreamPreciseShardTargeting\n  featureFlagCheckInvalidDatabaseInGlobalCatalog\n  featureFlagCheckRangeDeletionsWithMissingShardKeyIndex\n  featureFlagClusterFsyncLock\n  featureFlagClusterWideToaster\n  featureFlagClusteredConfigTransactions\n  featureFlagCompactOptions\n  featureFlagConcurrencyInChunkMigration\n  featureFlagConfigDebugDumpSupported\n  featureFlagCreateCollectionInPreparedTransactions\n  featureFlagCreateDatabaseDDLCoordinator\n  featureFlagCreateSpillKVEngine\n  featureFlagCreateViewlessTimeseriesCollections\n  featureFlagCurrentDate\n  featureFlagCursorBasedTop\n  featureFlagDatabaseDualCatalogCache\n  featureFlagDistinguishMetadataInconsistencyFromConflictingOperation\n  featureFlagDocumentSourceListCatalog\n  featureFlagDropIndexesDDLCoordinator\n  featureFlagDualCatalogCache\n  featureFlagEnableReplicasetTransitionToCSRS\n  featureFlagEndOfTransactionChangeEvent\n  featureFlagErrorAndLogValidationAction\n  featureFlagExposeClientIpInAuditLogs\n  featureFlagExtensionsAPI\n  featureFlagFork\n  featureFlagFryer\n  featureFlagGaplessFTDC\n  featureFlagInDevelopmentForTest\n  featureFlagIngressRateLimiting\n  featureFlagIntentRegistration\n  featureFlagInternalWritesAreReplicatedTransactionally\n  featureFlagLargeBatchedOperations\n  featureFlagLookupEncryptionSchemasFLE\n  featureFlagMandatoryAuthzChecks\n  featureFlagMongodProxyProtocolSupport\n  featureFlagMongotIndexedViews\n  featureFlagMoveCollection\n  featureFlagMqlJsEngineGap\n  featureFlagMultiPlanLimiter\n  featureFlagMultiServiceLogAndFTDCFormat\n  featureFlagOplogVisibility\n  featureFlagPQSBackfill\n  featureFlagPauseMigrationsDuringMultiUpdatesAvailable\n  featureFlagPersistRecipientPlacementInfoInMigrationRecoveryDoc\n  featureFlagPrefetch\n  featureFlagPrimaryDrivenIndexBuilds\n  featureFlagQETextSearchPreview\n  featureFlagQueryMemoryTracking\n  featureFlagQuerySettings\n  featureFlagQueryStats\n  featureFlagQueryStatsCountDistinct\n  featureFlagRankFusionBasic\n  featureFlagRankFusionFull\n  featureFlagRateLimitIngressConnectionEstablishment\n  featureFlagRawDataCrudOperations\n  featureFlagRearchitectUserAcquisition\n  featureFlagRecordDelinquentMetrics\n  featureFlagRecordIdsReplicated\n  featureFlagReduceMajorityWriteLatency\n  featureFlagRelaxedMode\n  featureFlagReleasedForTest\n  featureFlagRemoveLegacyTimeseriesBucketingParametersHaveChanged\n  featureFlagReplicaSetEndpoint\n  featureFlagReplicateLocalCatalogIdentifiers\n  featureFlagRequireTenantID\n  featureFlagReshardingAbortUnpreparedTransactionsUponPreparingToBlockWrites\n  featureFlagReshardingCloneNoRefresh\n  featureFlagReshardingForTimeseries\n  featureFlagReshardingNumSamplesPerChunk\n  featureFlagReshardingRemainingTimeEstimateBasedOnMovingAverage\n  featureFlagReshardingSkipCloningAndApplyingIfApplicable\n  featureFlagReshardingStoreOplogFetcherProgress\n  featureFlagReshardingVerification\n  featureFlagRouterPort\n  featureFlagSbeBlockHashAgg\n  featureFlagSbeFull\n  featureFlagSbeUpgradeBinaryTrees\n  featureFlagSearchBatchSizeTuning\n  featureFlagSearchExplainExecutionStats\n  featureFlagSearchHybridScoringFull\n  featureFlagSearchInSbe\n  featureFlagSecondaryIndexChecksInDbCheck\n  featureFlagSecurityToken\n  featureFlagServerlessChangeStreams\n  featureFlagSessionsCollectionCoordinatorOnConfigServer\n  featureFlagSetFcvDryRunMode\n  featureFlagShardAuthoritativeCollMetadata\n  featureFlagShardAuthoritativeDbMetadataCRUD\n  featureFlagShardAuthoritativeDbMetadataDDL\n  featureFlagShardFilteringDistinctScan\n  featureFlagShardedAggregationCatalogCacheGossiping\n  featureFlagSnapshotFCVInDDLCoordinators\n  featureFlagSpoon\n  featureFlagStorageEngineInterruptibility\n  featureFlagStreams\n  featureFlagTSBucketingParametersUnchanged\n  featureFlagTargetedMirrorReads\n  featureFlagTerminateSecondaryReadsUponRangeDeletion\n  featureFlagTimeSeriesInSbe\n  featureFlagTimeseriesDeletesSupport\n  featureFlagTimeseriesUpdatesSupport\n  featureFlagToaster\n  featureFlagTrackUnshardedCollectionsUponMoveCollection\n  featureFlagTransitionToCatalogShard\n  featureFlagUUIDExpression\n  featureFlagUnshardCollection\n  featureFlagUpdateDocumentShardKeyUsingTransactionApi\n  featureFlagUpgradingToDowngrading\n  featureFlagUseTopologyChangeCoordinators\n  featureFlagUseTwoStepRSToShardPromotion\n  featureFlagUseUnreplicatedTruncatesForDeletions\n  featureFlagVectorSearchPublicPreview\n  featureFlagVectorSimilarityExpressions\n  fileBasedInitialSyncExtendCursorTimeoutMS\n  fileBasedInitialSyncMaxCyclesWithoutProgress\n  fileBasedInitialSyncMaxLagSec\n  findChunksOnConfigTimeoutMS\n  flowControlDecayConstant\n  flowControlFudgeFactor\n  flowControlMaxSamples\n  flowControlMinTicketsPerSecond\n  flowControlSamplePeriod\n  flowControlTargetLagSeconds\n  flowControlThresholdLagPercentage\n  flowControlTicketAdderConstant\n  flowControlTicketMultiplierConstant\n  flowControlWarnThresholdSeconds\n  globalConnPoolIdleTimeoutMinutes\n  heartBeatFrequencyMs\n  honorSystemUmask\n  httpVerboseLogging\n  indexBuildMinAvailableDiskSpaceMB\n  indexMaxNumGeneratedKeysPerDocument\n  ingressAdmissionControlEnabled\n  ingressAdmissionControllerMaxQueueDepth\n  ingressAdmissionControllerTicketPoolSize\n  ingressConnectionEstablishmentBurstCapacitySecs\n  ingressConnectionEstablishmentMaxQueueDepth\n  ingressConnectionEstablishmentRateLimiterBypass\n  ingressConnectionEstablishmentRateLimiterEnabled\n  ingressConnectionEstablishmentRatePerSec\n  ingressRequestAdmissionBurstCapacitySecs\n  ingressRequestAdmissionRatePerSec\n  ingressRequestRateLimiterApplicationExemptions\n  ingressRequestRateLimiterEnabled\n  ingressRequestRateLimiterExemptions\n  initialSyncIndexBuildMemoryMaxMB\n  initialSyncIndexBuildMemoryMinMB\n  initialSyncIndexBuildMemoryPercentage\n  initialSyncMethod\n  initialSyncOplogBuffer\n  initialSyncOplogBufferPeekCacheSize\n  initialSyncOplogFetcherBatchSize\n  initialSyncSourceReadPreference\n  initialSyncTransientErrorRetryPeriodSeconds\n  internalChangeStreamUseTenantIdForTesting\n  internalDocumentSourceBucketAutoMaxMemoryBytes\n  internalDocumentSourceCursorBatchSizeBytes\n  internalDocumentSourceCursorInitialBatchSize\n  internalDocumentSourceDensifyMaxMemoryBytes\n  internalDocumentSourceGraphLookupMaxMemoryBytes\n  internalDocumentSourceGroupMaxMemoryBytes\n  internalDocumentSourceLookupCacheSizeBytes\n  internalDocumentSourceSetWindowFieldsMaxMemoryBytes\n  internalGeoNearQuery2DMaxCoveringCells\n  internalGeoPredicateQuery2DMaxCoveringCells\n  internalGraphLookupStageIntermediateDocumentMaxSizeBytes\n  internalIndexBuildBulkLoadYieldIterations\n  internalInsertMaxBatchSize\n  internalLookupStageIntermediateDocumentMaxSizeBytes\n  internalMaxSubPipelineViewDepth\n  internalMeasureQueryExecutionTimeInNanoseconds\n  internalPartialFilterExpressionMaxDepth\n  internalPipelineLengthLimit\n  internalProfilingMaxAbandonedWritesPerSecondPerDb\n  internalProhibitShardOperationRetry\n  internalQueryAggMulticastMaxConcurrency\n  internalQueryAggMulticastTimeoutMS\n  internalQueryAlwaysMergeOnPrimaryShard\n  internalQueryAppendIdToSetWindowFieldsSort\n  internalQueryAutoParameterizationMaxParameterCount\n  internalQueryBitTestPositionsLogThreshold\n  internalQueryCacheDisableInactiveEntries\n  internalQueryCacheEvictionRatio\n  internalQueryCacheMaxEntriesPerCollection\n  internalQueryCacheMaxSizeBytesBeforeStripDebugInfo\n  internalQueryCacheSize\n  internalQueryCacheWorksGrowthCoefficient\n  internalQueryCollectOptimizerMetrics\n  internalQueryCollectionMaxDataSizeBytesToChooseHashJoin\n  internalQueryCollectionMaxNoOfDocumentsToChooseHashJoin\n  internalQueryCollectionMaxStorageSizeBytesToChooseHashJoin\n  internalQueryConcurrentMultiPlanningThreshold\n  internalQueryDefaultDOP\n  internalQueryDesugarWhereToFunction\n  internalQueryDisableExchange\n  internalQueryDisableExclusionProjectionFastPath\n  internalQueryDisableLookupExecutionUsingHashJoin\n  internalQueryDisablePlanCache\n  internalQueryDisableSingleFieldExpressExecutor\n  internalQueryDoNotOpenContainedOrsInSimplifier\n  internalQueryDocumentSourceWriterBatchExtraReservedBytes\n  internalQueryEnableAggressiveSpillsInGroup\n  internalQueryEnableBooleanExpressionsSimplifier\n  internalQueryEnableCSTParser\n  internalQueryEnumerationMaxIntersectPerAnd\n  internalQueryEnumerationMaxOrSolutions\n  internalQueryEnumerationPreferLockstepOrEnumeration\n  internalQueryExecYieldIterations\n  internalQueryExecYieldPeriodMS\n  internalQueryExplainSizeThresholdBytes\n  internalQueryExpressionInterruptIterations\n  internalQueryExpressionInterruptPeriodMS\n  internalQueryExtraPredicateForReversedIn\n  internalQueryFLEAlwaysUseEncryptedCollScanMode\n  internalQueryFLERewriteMemoryLimit\n  internalQueryFacetBufferSizeBytes\n  internalQueryFacetMaxOutputDocSizeBytes\n  internalQueryFindCommandBatchSize\n  internalQueryForceIntersectionPlans\n  internalQueryFrameworkControl\n  internalQueryGetMoreMaxCursorPinRetryAttempts\n  internalQueryGlobalProfilingFilter\n  internalQueryGlobalProfilingLockDeadlineMs\n  internalQueryIgnoreUnknownJSONSchemaKeywords\n  internalQueryJavaScriptFnTimeoutMillis\n  internalQueryJavaScriptHeapSizeLimitMB\n  internalQueryMaxAddToSetBytes\n  internalQueryMaxAllowedDensifyDocs\n  internalQueryMaxBitTestIntermediatePositions\n  internalQueryMaxBlockingSortMemoryUsageBytes\n  internalQueryMaxConcatArraysBytes\n  internalQueryMaxConcurrentMultiPlanJobsPerCacheKey\n  internalQueryMaxDocValidationErrorConsideredValues\n  internalQueryMaxJsEmitBytes\n  internalQueryMaxMapReduceExpressionBytes\n  internalQueryMaxNumPrimeImplicants\n  internalQueryMaxPercentileAccumulatorBytes\n  internalQueryMaxPushBytes\n  internalQueryMaxRangeBytes\n  internalQueryMaxScansToExplode\n  internalQueryMaxSetUnionBytes\n  internalQueryMaxSizeFactorToSimplify\n  internalQueryMaxSpoolDiskUsageBytes\n  internalQueryMaxSpoolMemoryUsageBytes\n  internalQueryMaximumNumberOfMintermsInSimplifier\n  internalQueryMaximumNumberOfUniquePredicatesToSimplify\n  internalQueryNumChunksForChunkBasedSampling\n  internalQueryPercentileExprSelectToSortThreshold\n  internalQueryPermitMatchSwappingForComplexRenames\n  internalQueryPlanEvaluationCollFraction\n  internalQueryPlanEvaluationMaxResults\n  internalQueryPlanEvaluationWorks\n  internalQueryPlanOrChildrenIndependently\n  internalQueryPlanTieBreakingWithIndexHeuristics\n  internalQueryPlanTotalEvaluationCollFraction\n  internalQueryPlannerEnableHashIntersection\n  internalQueryPlannerEnableIndexIntersection\n  internalQueryPlannerEnableIndexPruning\n  internalQueryPlannerGenerateCoveredWholeIndexScans\n  internalQueryPlannerIgnoreIndexWithCollationForRegex\n  internalQueryPlannerMaxIndexedSolutions\n  internalQueryPlannerUseMultiplannerForSingleSolutions\n  internalQueryProhibitBlockingMergeOnMongoS\n  internalQueryProhibitMergingOnMongoS\n  internalQueryRegexHeapLimitKB\n  internalQueryRegexMatchLimit\n  internalQueryS2GeoCoarsestLevel\n  internalQueryS2GeoFinestLevel\n  internalQueryS2GeoMaxCells\n  internalQuerySBEPlanEvaluationMaxMemoryBytes\n  internalQuerySamplingBySequentialScan\n  internalQuerySamplingCEMethod\n  internalQuerySettingsBackfillDelaySeconds\n  internalQuerySettingsBackfillMemoryLimitBytes\n  internalQuerySlotBasedExecutionDisableGroupPushdown\n  internalQuerySlotBasedExecutionDisableLookupPushdown\n  internalQuerySlotBasedExecutionDisableTimeSeriesPushdown\n  internalQuerySlotBasedExecutionHashAggApproxMemoryUseInBytesBeforeSpill\n  internalQuerySlotBasedExecutionHashAggIncreasedSpilling\n  internalQuerySlotBasedExecutionHashAggMemoryCheckPerAdvanceAtLeast\n  internalQuerySlotBasedExecutionHashAggMemoryCheckPerAdvanceAtMost\n  internalQuerySlotBasedExecutionHashAggMemoryUseCheckMargin\n  internalQuerySlotBasedExecutionHashLookupApproxMemoryUseInBytesBeforeSpill\n  internalQuerySlotBasedExecutionMaxStaticIndexScanIntervals\n  internalQuerySlotBasedExecutionWindowBufferMemorySamplingAtLeast\n  internalQuerySlotBasedExecutionWindowStateMemorySamplingAtLeast\n  internalQuerySpillingMaxWaitTimeout\n  internalQuerySpillingMinAvailableDiskSpaceBytes\n  internalQueryStatsCacheSize\n  internalQueryStatsErrorsAreCommandFatal\n  internalQueryStatsRateLimit\n  internalQueryStatsSampleRate\n  internalQueryTdigestDelta\n  internalQueryTopNAccumulatorBytes\n  internalQueryUnifiedWriteExecutor\n  internalReduceAccumulatedValueDepthCheckInterval\n  internalRoaringBitmapsBatchSize\n  internalRoaringBitmapsMinimalDensity\n  internalRoaringBitmapsThreshold\n  internalSessionsReapThreshold\n  internalTextOrStageMaxMemoryBytes\n  internalValidateFeaturesAsMaster\n  internalValidateFeaturesAsPrimary\n  javascriptProtection\n  joinIngressSessionsOnShutdown\n  journalCommitInterval\n  jsHeapLimitMB\n  loadBalancerPort\n  loadRoutingTableOnStartup\n  localLogicalSessionTimeoutMinutes\n  localThresholdMs\n  logComponentVerbosity\n  logLevel\n  logicalSessionRefreshMillis\n  maxAcceptableLogicalClockDriftSecs\n  maxBSONDepth\n  maxCatchUpPercentageBeforeBlockingWrites\n  maxDbCheckMBperSec\n  maxIncomingConnectionsOverride\n  maxIndexBuildDrainBatchSize\n  maxIndexBuildDrainMemoryUsageMegabytes\n  maxIndexBuildMemoryUsageMegabytes\n  maxIteratorsMemoryUsagePercentage\n  maxLogSizeKB\n  maxNumActiveUserIndexBuilds\n  maxNumStaleVersionRetries\n  maxNumSyncSourceChangesPerHour\n  maxNumberOfBatchedOperationsInSingleOplogEntry\n  maxNumberOfInsertsBatchInsertsForRenameAcrossDatabases\n  maxNumberOfTransactionOperationsInSingleOplogEntry\n  maxOplogTruncationPointsAfterStartup\n  maxOplogTruncationPointsDuringStartup\n  maxRoundsWithoutProgressParameter\n  maxSessions\n  maxSizeOfBatchedInsertsForRenameAcrossDatabasesBytes\n  maxSizeOfBatchedOperationsInSingleOplogEntryBytes\n  maxSyncSourceLagSecs\n  maxTimeMSForHedgedReads\n  maxTransactionLockRequestTimeoutMillis\n  maxValidateMBperSec\n  maxValidateMemoryUsageMB\n  messageSizeErrorRateSec\n  metadataRefreshInTransactionMaxWaitBehindCritSecMS\n  metadataRefreshInTransactionMaxWaitMS\n  migrateCloneInsertionBatchDelayMS\n  migrateCloneInsertionBatchSize\n  migrationLockAcquisitionMaxWaitMS\n  minNumChunksForSessionsCollection\n  minOplogTruncationPoints\n  minSnapshotHistoryWindowInSeconds\n  minimalWriteConflictRetryCountForStateDump\n  mirrorReads\n  mirrorReadsMaxConnPoolSize\n  mongosShutdownTimeoutMillisForSignaledShutdown\n  mongotConnectionPoolMaxSize\n  mongotConnectionPoolMinSize\n  mongotHost\n  mrEnableSingleReduceOptimization\n  multiUpdateCoordinatorServiceMaxThreadCount\n  multiUpdateCoordinatorServiceMinThreadCount\n  multitenancySupport\n  newShardExistingClusterTimeKeysExpirationSecs\n  notablescan\n  numInitialSyncAttempts\n  numInitialSyncConnectAttempts\n  numInitialSyncOplogFindAttempts\n  ocspCacheSize\n  ocspEnabled\n  ocspStaplingRefreshPeriodSecs\n  ocspValidationRefreshPeriodSecs\n  openTelemetryMetricsDirectory\n  openTelemetryMetricsHttpEndpoint\n  opensslCipherConfig\n  opensslCipherSuiteConfig\n  opensslDiffieHellmanParameters\n  operationMemoryPoolBlockInitialSizeKB\n  operationMemoryPoolBlockMaxSizeKB\n  oplogApplicationEnforcesSteadyStateConstraints\n  oplogBatchDelayMillis\n  oplogFetcherInitialSyncMaxFetcherRestarts\n  oplogFetcherSteadyStateMaxFetcherRestarts\n  oplogFetcherUsesExhaust\n  oplogInitialFindMaxSeconds\n  oplogNetworkTimeoutBufferSeconds\n  oplogRetriedFindMaxSeconds\n  oplogSamplingAsyncEnabled\n  oplogSamplingAsyncYieldIntervalMs\n  oplogSamplingLogIntervalSeconds\n  oplogTruncationCheckPeriodSeconds\n  oplogTruncationPointSizeMB\n  opportunisticSecondaryTargeting\n  orphanCleanupDelaySecs\n  overdueInterruptCheckIntervalMillis\n  performTimeseriesCompressionIntermediateDataIntegrityCheckOnInsert\n  performTimeseriesCompressionIntermediateDataIntegrityCheckOnReopening\n  periodicNoopIntervalSecs\n  persistedChunkCacheUpdateMaxBatchSize\n  pessimisticConnectivityCheckForAcceptedConnections\n  pinTaskExecCursorConns\n  planCacheSize\n  planRankerMode\n  preAuthMaximumMessageSizeBytes\n  preImagesCollectionTruncateMarkersMinBytes\n  priorityTakeoverFreshnessWindowSeconds\n  processUmask\n  proxyProtocolMaximumPendingConnections\n  proxyProtocolMaximumWaitBackoffMillis\n  proxyProtocolTimeoutSecs\n  queryAnalysisQueryStatsSmoothingFactor\n  queryAnalysisSampleExpirationSecs\n  queryAnalysisSamplerBurstMultiplier\n  queryAnalysisSamplerConfigurationRefreshSecs\n  queryAnalysisSamplerInActiveThresholdSecs\n  queryAnalysisWriterIntervalSecs\n  queryAnalysisWriterMaxBatchSize\n  queryAnalysisWriterMaxMemoryUsageBytes\n  queryAnalysisWriterMaxThreadPoolSize\n  queryAnalysisWriterMinThreadPoolSize\n  quiet\n  rangeDeleterBatchDelayMS\n  rangeDeleterBatchSize\n  readHedgingMode\n  receiveChunkWaitForRangeDeleterTimeoutMS\n  recoverFromOplogAsStandalone\n  recoverToOplogTimestamp\n  redactEncryptedFields\n  remainingReshardingOperationTimePrepareThresholdMillis\n  remainingReshardingOperationTimeThresholdMillis\n  replBatchLimitBytes\n  replBatchLimitOperations\n  replElectionTimeoutOffsetLimitFraction\n  replWriterMinThreadCount\n  replWriterThreadCount\n  replicaSetMonitorProtocol\n  reportOpWriteConcernCountersInServerStatus\n  requireApiVersion\n  reshardingAbortUnpreparedTransactionsUponPreparingToBlockWrites\n  reshardingCollectionClonerBatchSizeCount\n  reshardingCollectionClonerMaxStalenessSeconds\n  reshardingCollectionClonerWriteThreadCount\n  reshardingCoordinatorQuiescePeriodMillis\n  reshardingCoordinatorServiceMaxThreadCount\n  reshardingCriticalSectionTimeoutMillis\n  reshardingDelayBeforeRemainingOperationTimeQueryMillis\n  reshardingDonorServiceMaxThreadCount\n  reshardingExponentialMovingAverageTimeToFetchAndApplyIntervalMillis\n  reshardingExponentialMovingAverageTimeToFetchAndApplySmoothingFactor\n  reshardingMaxDelayBetweenRemainingOperationTimeQueriesMillis\n  reshardingMaxReplicationLagSecondsBeforeBuildingIndexes\n  reshardingMinimumOperationDurationMillis\n  reshardingOplogApplierMaxLockRequestTimeoutMillis\n  reshardingOplogBatchLimitBytes\n  reshardingOplogBatchLimitOperations\n  reshardingOplogBatchTaskCount\n  reshardingOplogFetcherInsertBatchLimitBytes\n  reshardingOplogFetcherInsertBatchLimitOperations\n  reshardingOplogFetcherSleepMillisBeforeCriticalSection\n  reshardingOplogFetcherSleepMillisDuringCriticalSection\n  reshardingOplogFetcherTargetPrimaryDuringCriticalSection\n  reshardingRecipientServiceMaxThreadCount\n  reshardingRemainingTimeEstimateAccountsForDonorReplicationLag\n  reshardingRemainingTimeEstimateAccountsForRecipientReplicationLag\n  reshardingRemainingTimeEstimateBasedOnMovingAverage\n  reshardingTxnClonerProgressBatchSize\n  reshardingVerificationChangeStreamsEventsBatchSizeLimit\n  reshardingVerificationChangeStreamsEventsBatchTimeLimitSeconds\n  reshardingVerificationChangeStreamsSleepMS\n  reshardingWaitForReplicationIntervalMilliseconds\n  reshardingWaitForReplicationTimeoutSeconds\n  resumableIndexBuildMajorityOpTimeTimeoutMillis\n  rewriteStateChangeErrors\n  roleGraphInvalidationIsFatal\n  rollbackRemoteOplogQueryBatchSize\n  rollbackTimeLimitSecs\n  routingTableCacheChunkBucketSize\n  samplingConfidenceInterval\n  samplingMarginOfError\n  saslHostName\n  saslServiceName\n  saslauthdPath\n  scramIterationCount\n  scramSHA256IterationCount\n  scriptingEngineInterruptIntervalMS\n  searchIndexManagementHostAndPort\n  searchTLSMode\n  sessionMaxBatchSize\n  sessionWriteConcernTimeoutSystemMillis\n  shardedIndexConsistencyCheckIntervalMS\n  shutdownTimeoutMillisForSignaledShutdown\n  skipApplyingDbCheckBatchOnSecondary\n  skipAuthenticationToMongot\n  skipAuthenticationToSearchIndexManagementServer\n  skipCorruptDocumentsWhenCloning\n  skipDroppingHashedShardKeyIndex\n  skipOplogSampling\n  skipShardingConfigurationChecks\n  skipShellCursorFinalize\n  slowConnectionThresholdMillis\n  slowServiceOnStepDownThresholdMS\n  slowServiceOnStepUpBeginThresholdMS\n  slowServiceOnStepUpCompleteThresholdMS\n  slowTotalOnStepUpBeginThresholdMS\n  slowTotalOnStepUpCompleteThresholdMS\n  spTestNeedsFeatureFlagBlender\n  spTestNeedsFeatureFlagToaster\n  spTestNeedsLatestFCV\n  spillTableInsertBatchSizeBytes\n  spillWiredTigerBlockCompressor\n  spillWiredTigerCacheSizeMaxMB\n  spillWiredTigerCacheSizeMinMB\n  spillWiredTigerCacheSizePercentage\n  spillWiredTigerEngineConfig\n  spillWiredTigerEngineRuntimeConfig\n  spillWiredTigerEvictionDirtyTargetPercentage\n  spillWiredTigerEvictionDirtyTriggerPercentage\n  spillWiredTigerEvictionThreadsMax\n  spillWiredTigerEvictionThreadsMin\n  spillWiredTigerEvictionUpdatesTriggerPercentage\n  spillWiredTigerServerStatusVerbosity\n  spillWiredTigerSessionMax\n  spillWiredTigerZstdCompressionLevel\n  sslMode\n  sslWithholdClientCertificate\n  startupAuthSchemaValidation\n  startupRecoveryForRestore\n  storageEngineConcurrencyAdjustmentAlgorithm\n  storageEngineConcurrencyAdjustmentIntervalMillis\n  storageEngineConcurrentReadTransactions\n  storageEngineConcurrentWriteTransactions\n  storageEngineReadMaxQueueDepth\n  storageEngineWriteMaxQueueDepth\n  storageGlobalParams.directoryperdb\n  supportArbitraryClusterKeyIndex\n  suppressNoTLSPeerCertificateWarning\n  syncdelay\n  takeUnstableCheckpointOnShutdown\n  taskExecutorPoolSize\n  tcpFastOpenClient\n  tcpFastOpenQueueSize\n  tcpFastOpenServer\n  temporarilyUnavailableBackoffBaseMs\n  temporarilyUnavailableMaxRetries\n  terminateSecondaryReadsOnOrphanCleanup\n  testMongosOnlyRuntimeParameter\n  testOnlyValidatedTenancyScopeKey\n  testingDiagnosticsEnabled\n  testingSnapshotBehaviorInIsolation\n  throughputProbingConcurrencyMovingAverageWeight\n  throughputProbingInitialConcurrency\n  throughputProbingMaxConcurrency\n  throughputProbingMinConcurrency\n  throughputProbingReadWriteRatio\n  throughputProbingStallDetectionTimeoutMs\n  throughputProbingStepMultiple\n  timeseriesBucketMaxCount\n  timeseriesBucketMaxSize\n  timeseriesBucketMinCount\n  timeseriesBucketMinSize\n  timeseriesIdleBucketExpiryMaxCountPerAttempt\n  timeseriesIdleBucketExpiryMemoryUsageThreshold\n  timeseriesInsertMaxRetriesOnDuplicates\n  timeseriesLargeMeasurementThreshold\n  timeseriesMaxRetriesForWriteConflictsOnReopening\n  timeseriesSideBucketCatalogMemoryUsageThreshold\n  timeseriesValidateCompressedBuckets\n  tlsClusterAuthX509Override\n  tlsMode\n  tlsOCSPSlowResponderWarningSecs\n  tlsOCSPStaplingTimeoutSecs\n  tlsOCSPVerifyTimeoutSecs\n  tlsUseSystemCA\n  tlsWithholdClientCertificate\n  tlsX509ClusterAuthDNOverride\n  tlsX509ExpirationWarningThresholdDays\n  traceExceptions\n  traceWriteConflictExceptions\n  trafficRecordingDirectory\n  transactionLifetimeLimitSeconds\n  transactionSizeLimitBytes\n  transactionTooLargeForCacheThreshold\n  ttlIndexDeleteTargetDocs\n  ttlIndexDeleteTargetTimeMS\n  ttlMonitorBatchDeletes\n  ttlMonitorEnabled\n  ttlMonitorMaxMetadataRecoveryThreads\n  ttlMonitorSleepSecs\n  ttlMonitorSubPassTargetSecs\n  unsupportedDangerousTestingFLEDiagnosticsEnabled\n  unsupportedSyncSource\n  upsertMaxRetryAttemptsOnDuplicateKeyError\n  useBatchedDeletesForRangeDeletion\n  useGrpcForSearch\n  useReadOnceCursorsForIndexBuilds\n  useSlowCollectionTruncateMarkerScanning\n  userCacheInvalidationIntervalSecs\n  validateCollectionName\n  validateDbName\n  waitForSecondaryBeforeNoopWriteMS\n  warmMinConnectionsInShardingTaskExecutorPoolOnStartup\n  warmMinConnectionsInShardingTaskExecutorPoolOnStartupWaitMS\n  watchdogPeriodSeconds\n  wiredTigerCheckpointCleanupPeriodSeconds\n  wiredTigerConcurrentReadTransactions\n  wiredTigerConcurrentWriteTransactions\n  wiredTigerCursorCacheSize\n  wiredTigerDirectoryForIndexes\n  wiredTigerEngineRuntimeConfig\n  wiredTigerEvictionDirtyMaxGB\n  wiredTigerEvictionDirtyTargetGB\n  wiredTigerEvictionUpdatesMaxGB\n  wiredTigerExtraDiagnostics\n  wiredTigerFileHandleCloseIdleTime\n  wiredTigerFileHandleCloseMinimum\n  wiredTigerFileHandleCloseScanInterval\n  wiredTigerSessionCacheMaxPercentage\n  wiredTigerSessionCloseIdleTimeSecs\n  wiredTigerSessionMax\n  wiredTigerSizeStorerPeriodicSyncHits\n  wiredTigerSizeStorerPeriodicSyncPeriodMillis\n  wiredTigerSkipTableLoggingChecksDuringValidation\n  wiredTigerSkipTableLoggingChecksOnStartup\n  wiredTigerStressConfig\n  wiredTigerVerboseShutdownCheckpointLogs\n  writeConflictRetryCountForDumpState\n  writeConflictRetryLimit\n  writePeriodicNoops\n",
    ),
    (
        "setProfilingFilterGlobally",
        SECONDARY_OK | REQUIRES_AUTH,
        "updates a global filter that determines which operations are eligible for logging/profiling",
    ),
    (
        "setQuerySettings",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Sets the query settings for the query shape of a given query.",
    ),
    (
        "setUserWriteBlockMode",
        REQUIRES_AUTH,
        "Set whether user write blocking is enabled",
    ),
    (
        "shardingState",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "shutdown",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "Shuts down the database. Must be run against the admin database and either (1) run from localhost or (2) run while authenticated with the shutdown privilege. If the node is the primary of a replica set, waits up to 'timeoutSecs' for an electable node to be caught up before stepping down. If 'force' is false and no electable node was able to catch up, does not shut down. If the node is in state SECONDARY after the attempted stepdown, any remaining time in 'timeout' is used for quiesce mode, where the database continues to allow operations to run, but directs clients to route new operations to other replica set members.",
    ),
    (
        "splitChunk",
        ADMIN_ONLY | REQUIRES_AUTH,
        "internal command usage only\nexample:\n { splitChunk:\"db.foo\" , keyPattern: {a:1} , min : {a:100} , max: {a:200} { splitKeys : [ {a:150} , ... ]}",
    ),
    (
        "splitVector",
        REQUIRES_AUTH,
        "Internal command.\nexamples:\n  { splitVector : \"blog.post\" , keyPattern:{x:1} , min:{x:10} , max:{x:20}, maxChunkSize:200 }\n  maxChunkSize unit in MBs\n  May optionally specify 'maxSplitPoints' and 'maxChunkObjects' to avoid traversing the whole chunk\n  \n  { splitVector : \"blog.post\" , keyPattern:{x:1} , min:{x:10} , max:{x:20}, force: true }\n  'force' will produce one split point even if data is small; defaults to false\nNOTE: This command may take a while to run",
    ),
    (
        "startSession",
        SECONDARY_OK | REQUIRES_AUTH,
        "start a logical session",
    ),
    (
        "startTrafficRecording",
        SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "stopTrafficRecording",
        SECONDARY_OK | REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "top",
        ADMIN_ONLY | SECONDARY_OK | REQUIRES_AUTH,
        "usage by collection, in micros ",
    ),
    (
        "transitionToShardedCluster",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Transitions a replica set into a sharded cluster.",
    ),
    (
        "update",
        REQUIRES_AUTH | API_V1,
        "update documents",
    ),
    (
        "updateRole",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "updateSearchIndex",
        REQUIRES_AUTH,
        "Command to update a search index. Only supported with Atlas.",
    ),
    (
        "updateUser",
        REQUIRES_AUTH,
        "no help defined",
    ),
    (
        "usersInfo",
        REQUIRES_AUTH | SECONDARY_OVERRIDE_OK,
        "no help defined",
    ),
    (
        "validate",
        SECONDARY_OK | REQUIRES_AUTH,
        "Validate contents of a namespace by scanning its data structures for correctness.\nThis is a slow operation.\n\tAdd {full: true} option to do a more thorough check.\n\tAdd {background: true} to validate in the background.\n\tAdd {repair: true} to run repair mode.\n\tAdd {fixMultikey: true} to fix the multi key.\n\tAdd {checkBSONConformance: true} to validate BSON documents more thoroughly.\n\tAdd {metadata: true} to only check collection metadata.\nCannot specify both {full: true, background: true}.",
    ),
    (
        "validateDBMetadata",
        SECONDARY_OK | REQUIRES_AUTH,
        "validateDBMetadata checks that the stored metadata of a database/collection is valid within a particular API version. If 'db' parameter is specified, only runs validation against that database, if not the validation will be run againt all dbs. Similarly if 'collection' parameter is specified, the validation is only run against that collection, if not the validation is run against all collections.",
    ),
    (
        "voteAbortIndexBuild",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Internal intra replica set command to request that the primary abort an index build with the specified UUID.",
    ),
    (
        "voteCommitIndexBuild",
        ADMIN_ONLY | REQUIRES_AUTH,
        "Internal intra replica set command to signal to the primary that a member is ready to commit an index build.",
    ),
    (
        "whatsmyuri",
        SECONDARY_OK | REQUIRES_AUTH,
        "{whatsmyuri:1}",
    ),
];

/// mongod's row for `name`, exactly as spelled.
pub(crate) fn row(name: &str) -> Option<&'static (&'static str, u8, &'static str)> {
    COMMANDS
        .binary_search_by(|(known, _, _)| (*known).cmp(name))
        .ok()
        .map(|i| &COMMANDS[i])
}

/// Whether mongod runs `name` only against the `admin` database.
pub(crate) fn admin_only(name: &str) -> bool {
    row(name).is_some_and(|(_, flags, _)| flags & ADMIN_ONLY != 0)
}

/// Whether `name` is in API Version 1.
pub(crate) fn api_v1(name: &str) -> bool {
    row(name).is_some_and(|(_, flags, _)| flags & API_V1 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_so_the_search_finds_every_row() {
        assert!(COMMANDS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(admin_only("getLog") && !admin_only("ping"));
        assert!(api_v1("find") && !api_v1("collStats"));
        assert!(row("nosuch").is_none());
    }
}
